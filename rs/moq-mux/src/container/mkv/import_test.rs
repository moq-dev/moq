//! Tests for the MKV/WebM importer.
//!
//! These tests synthesize small WebM files via webm-iterable's writer (no external
//! tooling required) and feed them through [`crate::container::mkv::Import`], then assert that
//! the resulting catalog and frame stream are well-formed.

use std::io::Cursor;

use hang::catalog::{AudioCodec, Container, VideoCodec};
use webm_iterable::WebmWriter;
use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};

/// Build a minimal WebM byte stream with the given tracks and blocks.
struct MkvBuilder {
	tags: Vec<MatroskaSpec>,
}

impl MkvBuilder {
	fn new() -> Self {
		Self { tags: Vec::new() }
	}

	fn header(mut self, doc_type: &str) -> Self {
		self.tags.push(MatroskaSpec::Ebml(Master::Full(vec![
			MatroskaSpec::DocType(doc_type.to_string()),
			MatroskaSpec::DocTypeVersion(2),
			MatroskaSpec::DocTypeReadVersion(2),
		])));
		self
	}

	fn segment_start(mut self) -> Self {
		self.tags.push(MatroskaSpec::Segment(Master::Start));
		self
	}

	fn segment_end(mut self) -> Self {
		self.tags.push(MatroskaSpec::Segment(Master::End));
		self
	}

	fn info(mut self, timestamp_scale_ns: u64) -> Self {
		self.tags
			.push(MatroskaSpec::Info(Master::Full(vec![MatroskaSpec::TimestampScale(
				timestamp_scale_ns,
			)])));
		self
	}

	fn track_video(mut self, number: u64, codec_id: &str, width: u64, height: u64) -> Self {
		self.tags
			.push(MatroskaSpec::Tracks(Master::Full(vec![MatroskaSpec::TrackEntry(
				Master::Full(vec![
					MatroskaSpec::TrackNumber(number),
					MatroskaSpec::TrackUID(number),
					MatroskaSpec::TrackType(1),
					MatroskaSpec::CodecID(codec_id.to_string()),
					MatroskaSpec::Video(Master::Full(vec![
						MatroskaSpec::PixelWidth(width),
						MatroskaSpec::PixelHeight(height),
					])),
				]),
			)])));
		self
	}

	fn tracks(mut self, entries: Vec<MatroskaSpec>) -> Self {
		self.tags.push(MatroskaSpec::Tracks(Master::Full(entries)));
		self
	}

	fn cluster<F>(mut self, cluster_timestamp: u64, blocks: F) -> Self
	where
		F: FnOnce() -> Vec<MatroskaSpec>,
	{
		self.tags.push(MatroskaSpec::Cluster(Master::Start));
		self.tags.push(MatroskaSpec::Timestamp(cluster_timestamp));
		self.tags.extend(blocks());
		self.tags.push(MatroskaSpec::Cluster(Master::End));
		self
	}

	fn build(self) -> Vec<u8> {
		let mut dest = Cursor::new(Vec::new());
		{
			let mut writer = WebmWriter::new(&mut dest);
			for tag in &self.tags {
				writer.write(tag).expect("write tag");
			}
		}
		dest.into_inner()
	}
}

fn simple_block(track: u64, rel_ts: i16, keyframe: bool, payload: &[u8]) -> MatroskaSpec {
	let sb = SimpleBlock::new_uncheked(payload, track, rel_ts, false, None, false, keyframe);
	sb.into()
}

fn track_entry_audio_opus(number: u64, sample_rate: f64, channels: u64) -> MatroskaSpec {
	// Minimal OpusHead: magic + version + channels + pre-skip + sample_rate (LE) + gain + mapping.
	let mut head = Vec::new();
	head.extend_from_slice(b"OpusHead");
	head.push(1); // version
	head.push(channels as u8);
	head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
	head.extend_from_slice(&(sample_rate as u32).to_le_bytes());
	head.extend_from_slice(&0i16.to_le_bytes()); // gain
	head.push(0); // mapping family

	MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(number),
		MatroskaSpec::TrackUID(number),
		MatroskaSpec::TrackType(2),
		MatroskaSpec::CodecID("A_OPUS".to_string()),
		MatroskaSpec::CodecPrivate(head),
		MatroskaSpec::Audio(Master::Full(vec![
			MatroskaSpec::SamplingFrequency(sample_rate),
			MatroskaSpec::Channels(channels),
		])),
	]))
}

fn track_entry_audio_flac(number: u64, sample_rate: u32, channels: u32) -> MatroskaSpec {
	// A_FLAC CodecPrivate is the FLAC header: the `fLaC` marker plus the STREAMINFO
	// metadata block. Reuse the codec helper so the bytes match what the importer parses.
	let private = crate::codec::flac::Config {
		min_block_size: 4096,
		max_block_size: 4096,
		min_frame_size: 0,
		max_frame_size: 0,
		sample_rate,
		channel_count: channels,
		bits_per_sample: 16,
		total_samples: 0,
		md5: [0; 16],
	}
	.description()
	.to_vec();

	MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(number),
		MatroskaSpec::TrackUID(number),
		MatroskaSpec::TrackType(2),
		MatroskaSpec::CodecID("A_FLAC".to_string()),
		MatroskaSpec::CodecPrivate(private),
		MatroskaSpec::Audio(Master::Full(vec![
			MatroskaSpec::SamplingFrequency(sample_rate as f64),
			MatroskaSpec::Channels(channels as u64),
		])),
	]))
}

fn track_entry_audio_mp3(number: u64, sample_rate: f64, channels: u64) -> MatroskaSpec {
	// MP3 has no codec private; config comes from the track header.
	MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(number),
		MatroskaSpec::TrackUID(number),
		MatroskaSpec::TrackType(2),
		MatroskaSpec::CodecID("A_MPEG/L3".to_string()),
		MatroskaSpec::Audio(Master::Full(vec![
			MatroskaSpec::SamplingFrequency(sample_rate),
			MatroskaSpec::Channels(channels),
		])),
	]))
}

fn track_entry_video_vp9(number: u64, width: u64, height: u64) -> MatroskaSpec {
	MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(number),
		MatroskaSpec::TrackUID(number),
		MatroskaSpec::TrackType(1),
		MatroskaSpec::CodecID("V_VP9".to_string()),
		MatroskaSpec::Video(Master::Full(vec![
			MatroskaSpec::PixelWidth(width),
			MatroskaSpec::PixelHeight(height),
		])),
	]))
}

fn run(data: &[u8]) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut mkv = crate::container::mkv::Import::new(broadcast, catalog.reserve());
	let buf = bytes::BytesMut::from(data);
	mkv.decode(&buf).expect("decode");
	mkv.finish().expect("finish");
	catalog.snapshot()
}

// A supported malformed track must fail the import, even beside a usable track.
fn assert_out_of_range(track: MatroskaSpec, field: &str) {
	for reverse in [false, true] {
		let mut tracks = vec![track_entry_audio_opus(1, 48_000.0, 2), track.clone()];
		if reverse {
			tracks.reverse();
		}
		let data = MkvBuilder::new()
			.header("matroska")
			.segment_start()
			.info(1_000_000)
			.tracks(tracks)
			.segment_end()
			.build();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let result = import.decode(&data).and_then(|()| import.finish());
		assert!(
			matches!(&result, Err(crate::Error::Mkv(super::Error::ValueOutOfRange(name))) if *name == field),
			"{field}, reverse {reverse}: {result:?}"
		);
	}
}

#[test]
fn numeric_range_pixel_width() {
	for value in [u64::from(u32::MAX) + 1, u64::MAX] {
		assert_out_of_range(track_entry_video_vp9(2, value, 1080), "PixelWidth");
	}
}

#[test]
fn numeric_range_pixel_height() {
	for value in [u64::from(u32::MAX) + 1, u64::MAX] {
		assert_out_of_range(track_entry_video_vp9(2, 1920, value), "PixelHeight");
	}
}

#[test]
fn numeric_range_channels() {
	for value in [u64::from(u32::MAX) + 1, u64::MAX] {
		assert_out_of_range(track_entry_audio_mp3(2, 48_000.0, value), "Channels");
	}
}

#[test]
fn numeric_range_sampling_frequency() {
	for value in [
		f64::from(u32::MAX) + 0.5,
		f64::from(u32::MAX) + 1.0,
		-1.0,
		f64::NAN,
		f64::INFINITY,
		f64::NEG_INFINITY,
	] {
		assert_out_of_range(track_entry_audio_mp3(2, value, 2), "SamplingFrequency");
	}
}

#[test]
fn numeric_range_representable_values_keep_existing_conversion() {
	for (size, rate, channels) in [
		(0, 0.0, 0),
		(u32::MAX, f64::from(u32::MAX), u32::MAX),
		(1920, 44_100.5, 2),
	] {
		let data = MkvBuilder::new()
			.header("matroska")
			.segment_start()
			.info(1_000_000)
			.tracks(vec![
				track_entry_video_vp9(1, u64::from(size), u64::from(size)),
				track_entry_audio_mp3(2, rate, u64::from(channels)),
			])
			.segment_end()
			.build();
		let catalog = run(&data);
		let video = catalog.video.renditions.values().next().unwrap();
		assert_eq!((video.coded_width, video.coded_height), (Some(size), Some(size)));
		let audio = catalog.audio.renditions.values().next().unwrap();
		assert_eq!((audio.sample_rate, audio.channel_count), (rate as u32, channels));
	}
}

#[test]
fn numeric_range_unsupported_codecs_are_refused() {
	use MatroskaSpec::*;
	for (kind, codec, fields) in [
		(
			1,
			"V_UNKNOWN",
			Video(Master::Full(vec![PixelWidth(u64::MAX), PixelHeight(u64::MAX)])),
		),
		(
			2,
			"A_UNKNOWN",
			Audio(Master::Full(vec![Channels(u64::MAX), SamplingFrequency(f64::INFINITY)])),
		),
	] {
		let unsupported = TrackEntry(Master::Full(vec![
			TrackNumber(2),
			TrackType(kind),
			CodecID(codec.into()),
			fields,
		]));
		for reverse in [false, true] {
			let mut tracks = vec![track_entry_audio_opus(1, 48_000.0, 2), unsupported.clone()];
			if reverse {
				tracks.reverse();
			}
			let data = MkvBuilder::new()
				.header("matroska")
				.segment_start()
				.info(1_000_000)
				.tracks(tracks)
				.segment_end()
				.build();
			for initial in [false, true] {
				let error = import_result(&data, initial).unwrap_err();
				assert!(
					matches!(&error,
						crate::Error::Mkv(super::Error::UnsupportedVideoCodec(name) | super::Error::UnsupportedAudioCodec(name)) if name == codec
					),
					"{codec}, reverse {reverse}, initial {initial}: {error:?}"
				);
			}
		}
	}
}

#[test]
fn numeric_range_validates_fields_without_overriding_codec_config() {
	use crate::codec::h264;
	use MatroskaSpec::*;
	use bytes::Bytes;

	let avcc = h264::build_avcc(
		&[Bytes::from_static(h264::fixtures::SPS_IPB)],
		&[Bytes::from_static(h264::fixtures::PPS)],
	)
	.unwrap();
	let video = TrackEntry(Master::Full(vec![
		TrackNumber(2),
		TrackType(1),
		CodecID("V_MPEG4/ISO/AVC".into()),
		CodecPrivate(avcc.to_vec()),
		Video(Master::Full(vec![PixelWidth(1920), PixelHeight(1080)])),
	]));
	let aac = TrackEntry(Master::Full(vec![
		TrackNumber(2),
		TrackType(2),
		CodecID("A_AAC".into()),
		CodecPrivate(vec![0x11, 0x90]),
		Audio(Master::Full(vec![])),
	]));
	for (track, is_video) in [
		(video, true),
		(track_entry_audio_opus(2, 48_000.0, 2), false),
		(track_entry_audio_flac(2, 48_000, 2), false),
		(aac, false),
	] {
		let cases = if is_video {
			vec![
				(vec![PixelWidth(1920), PixelHeight(1080)], None),
				(vec![PixelWidth(u64::MAX), PixelHeight(1080)], Some("PixelWidth")),
				(vec![PixelWidth(1920), PixelHeight(u64::MAX)], Some("PixelHeight")),
			]
		} else {
			vec![
				(vec![SamplingFrequency(44_100.0), Channels(6)], None),
				(
					vec![SamplingFrequency(f64::INFINITY), Channels(2)],
					Some("SamplingFrequency"),
				),
				(vec![SamplingFrequency(48_000.0), Channels(u64::MAX)], Some("Channels")),
			]
		};
		for (fields, error) in cases {
			let mut track = track.clone();
			let TrackEntry(Master::Full(children)) = &mut track else {
				unreachable!()
			};
			for child in children {
				if let Video(Master::Full(value)) | Audio(Master::Full(value)) = child {
					*value = fields.clone();
				}
			}
			if let Some(field) = error {
				assert_out_of_range(track, field);
				continue;
			}
			let data = MkvBuilder::new()
				.header("matroska")
				.segment_start()
				.info(1_000_000)
				.tracks(vec![track])
				.segment_end()
				.build();
			let catalog = run(&data);
			if is_video {
				let video = catalog.video.renditions.values().next().unwrap();
				assert_eq!((video.coded_width, video.coded_height), (Some(64), Some(64)));
			} else {
				let audio = catalog.audio.renditions.values().next().unwrap();
				assert_eq!((audio.sample_rate, audio.channel_count), (48_000, 2));
			}
		}
	}
}

#[test]
fn codec_private_resolves_display_aspect() {
	use crate::codec::{h264, h265};
	use bytes::Bytes;
	for (avc, hevc, expected) in [
		(h264::fixtures::SPS_IPB, h265::fixtures::SPS_NVENC, (None, None)),
		(
			h264::fixtures::SPS_ANAMORPHIC,
			h265::fixtures::SPS_ANAMORPHIC,
			(Some(16), Some(9)),
		),
	] {
		let avcc = h264::build_avcc(&[Bytes::from_static(avc)], &[Bytes::from_static(h264::fixtures::PPS)]).unwrap();
		let hvcc = h265::build_hvcc(
			&[Bytes::from_static(h265::fixtures::VPS)],
			&[Bytes::from_static(hevc)],
			&[Bytes::from_static(h265::fixtures::PPS)],
		)
		.unwrap();
		for (codec_id, private) in [("V_MPEG4/ISO/AVC", avcc), ("V_MPEGH/ISO/HEVC", hvcc)] {
			let (width, height) = match expected {
				(None, None) if codec_id == "V_MPEG4/ISO/AVC" => (64, 64),
				(None, None) => (1280, 720),
				_ => (1440, 1080),
			};
			let data = MkvBuilder::new()
				.header("matroska")
				.segment_start()
				.info(1_000_000)
				.tracks(vec![MatroskaSpec::TrackEntry(Master::Full(vec![
					MatroskaSpec::TrackNumber(1),
					MatroskaSpec::TrackUID(1),
					MatroskaSpec::TrackType(1),
					MatroskaSpec::CodecID(codec_id.into()),
					MatroskaSpec::CodecPrivate(private.to_vec()),
					MatroskaSpec::Video(Master::Full(vec![
						MatroskaSpec::PixelWidth(width),
						MatroskaSpec::PixelHeight(height),
					])),
				]))])
				.segment_end()
				.build();
			let catalog = run(&data);
			let config = catalog.video.renditions.values().next().unwrap();
			assert_eq!(
				(config.display_aspect_width, config.display_aspect_height),
				expected,
				"{codec_id}"
			);
		}
	}
}

fn aspect_file(codec_id: &str, private: Option<Vec<u8>>, extra: Vec<MatroskaSpec>) -> Vec<u8> {
	let mut video = vec![MatroskaSpec::PixelWidth(1440), MatroskaSpec::PixelHeight(1080)];
	video.extend(extra);
	let mut track = vec![
		MatroskaSpec::TrackNumber(1),
		MatroskaSpec::TrackUID(1),
		MatroskaSpec::TrackType(1),
		MatroskaSpec::CodecID(codec_id.into()),
		MatroskaSpec::Video(Master::Full(video)),
	];
	if let Some(private) = private {
		track.push(MatroskaSpec::CodecPrivate(private));
	}
	MkvBuilder::new()
		.header("matroska")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![MatroskaSpec::TrackEntry(Master::Full(track))])
		.segment_end()
		.build()
}

#[test]
fn container_aspect_display_dimensions_and_crop() {
	use MatroskaSpec::*;
	let cases = [
		(vec![DisplayWidth(1920), DisplayHeight(1080)], (Some(16), Some(9))),
		(vec![DisplayWidth(1080), DisplayHeight(1080)], (Some(1), Some(1))),
		(vec![DisplayWidth(1920)], (Some(16), Some(9))),
		(vec![DisplayHeight(810)], (Some(16), Some(9))),
		(vec![], (None, None)),
		(vec![DisplayWidth(2880), DisplayHeight(2160)], (None, None)),
		// The catalog has no crop, so a crop with default display dimensions keeps square samples
		// and the whole picture keeps its shape instead of being squeezed into the cropped ratio.
		(vec![PixelCropRight(360)], (None, None)),
		(vec![PixelCropLeft(180), PixelCropRight(180)], (None, None)),
		(vec![PixelCropTop(135), PixelCropBottom(135)], (None, None)),
		(vec![PixelCropRight(720), PixelCropBottom(540)], (None, None)),
		(
			vec![PixelCropRight(360), DisplayWidth(1080), DisplayHeight(1080)],
			(None, None),
		),
		// A 1080x1080 crop displayed at 1440x1080 has 4:3 samples, so the whole 1440x1080 is 16:9.
		(
			vec![PixelCropRight(360), DisplayWidth(1440), DisplayHeight(1080)],
			(Some(16), Some(9)),
		),
		(vec![PixelCropRight(360), DisplayWidth(1440)], (Some(16), Some(9))),
		(
			vec![PixelCropRight(360), DisplayWidth(1920), DisplayHeight(1080)],
			(Some(64), Some(27)),
		),
	];
	for (extra, expected) in cases {
		// AV1 has no SPS aspect; VP9 uses the same container metadata path.
		for (codec, private) in [("V_AV1", Some(vec![0x81, 0, 0, 0])), ("V_VP9", None)] {
			let catalog = run(&aspect_file(codec, private, extra.clone()));
			let config = catalog.video.renditions.values().next().unwrap();
			assert_eq!(
				(config.display_aspect_width, config.display_aspect_height),
				expected,
				"{codec} {extra:?}"
			);
		}
	}
	for unit in 1..=4 {
		let data = aspect_file(
			"V_VP9",
			None,
			vec![DisplayUnit(unit), DisplayWidth(16), DisplayHeight(9)],
		);
		let catalog = run(&data);
		let config = catalog.video.renditions.values().next().unwrap();
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(Some(16), Some(9)),
			"unit {unit}"
		);
	}
}

fn assert_invalid_aspect(data: &[u8], context: &str) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut import = super::Import::new(broadcast, catalog.reserve());
	let result = import.decode(data).and_then(|()| import.finish());
	assert!(
		matches!(result, Err(crate::Error::Mkv(super::Error::InvalidAspect(_)))),
		"{context}: {result:?}"
	);
}

#[test]
fn container_aspect_refuses_missing_nonpixel_dimensions() {
	use MatroskaSpec::*;
	for unit in 1..=4 {
		for extra in [vec![], vec![DisplayWidth(16)], vec![DisplayHeight(9)]] {
			let mut fields = vec![DisplayUnit(unit)];
			fields.extend(extra);
			assert_invalid_aspect(&aspect_file("V_VP9", None, fields), &format!("unit {unit}"));
		}
	}
}

#[test]
fn container_aspect_malformed_track_is_not_skipped() {
	use MatroskaSpec::*;
	for reverse in [false, true] {
		let mut tracks = vec![
			track_entry_audio_opus(1, 48000.0, 2),
			TrackEntry(Master::Full(vec![
				TrackNumber(2),
				TrackUID(2),
				TrackType(1),
				CodecID("V_VP9".into()),
				Video(Master::Full(vec![
					PixelWidth(1440),
					PixelHeight(1080),
					DisplayUnit(1),
					DisplayWidth(16),
				])),
			])),
		];
		if reverse {
			tracks.reverse();
		}
		let data = MkvBuilder::new()
			.header("matroska")
			.segment_start()
			.info(1_000_000)
			.tracks(tracks)
			.segment_end()
			.build();
		assert_invalid_aspect(&data, &format!("reverse {reverse}"));
	}
}

#[tokio::test]
async fn container_aspect_unsupported_role_keeps_audio() {
	use MatroskaSpec::*;
	for reverse in [false, true] {
		let mut tracks = vec![
			track_entry_audio_opus(1, 48000.0, 2),
			TrackEntry(Master::Full(vec![
				TrackNumber(2),
				TrackUID(2),
				TrackType(18),
				CodecID("V_UNKNOWN".into()),
				Video(Master::Full(vec![PixelWidth(0), PixelHeight(1080)])),
			])),
		];
		if reverse {
			tracks.reverse();
		}
		let data = MkvBuilder::new()
			.header("matroska")
			.segment_start()
			.info(1_000_000)
			.tracks(tracks)
			.cluster(0, || vec![simple_block(1, 0, true, b"opus")])
			.segment_end()
			.build();
		let catalog = run(&data);
		assert_eq!(catalog.audio.renditions.len(), 1);
		assert!(catalog.video.renditions.is_empty());
		let imported = import_chunked(&data, data.len()).await;
		assert!(imported.refused.is_none());
		assert_eq!(imported.published.len(), 1);
		assert_eq!(imported.published.values().next().unwrap(), &[0]);
	}
}

#[test]
#[tracing_test::traced_test]
fn container_aspect_matroska_keeps_sps() {
	use crate::codec::{h264, h265};
	use bytes::Bytes;
	let avcc = h264::build_avcc(
		&[Bytes::from_static(h264::fixtures::SPS_ANAMORPHIC)],
		&[Bytes::from_static(h264::fixtures::PPS)],
	)
	.unwrap();
	let hvcc = h265::build_hvcc(
		&[Bytes::from_static(h265::fixtures::VPS)],
		&[Bytes::from_static(h265::fixtures::SPS_ANAMORPHIC)],
		&[Bytes::from_static(h265::fixtures::PPS)],
	)
	.unwrap();
	for (codec, private) in [("V_MPEG4/ISO/AVC", avcc), ("V_MPEGH/ISO/HEVC", hvcc)] {
		let catalog = run(&aspect_file(
			codec,
			Some(private.to_vec()),
			vec![MatroskaSpec::DisplayWidth(1080), MatroskaSpec::DisplayHeight(1080)],
		));
		let config = catalog.video.renditions.values().next().unwrap();
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(Some(16), Some(9))
		);
	}
	logs_assert(|lines| {
		let conflicts = lines
			.iter()
			.filter(|line| line.contains("display aspect disagrees") && line.contains("Matroska"))
			.count();
		if conflicts == 2 {
			Ok(())
		} else {
			Err(format!("expected two SPS conflicts, got {conflicts}"))
		}
	});
}

#[test]
fn container_aspect_absent_preserves_pixel_dimensions() {
	use MatroskaSpec::*;
	for (fields, expected) in [
		(vec![], (None, None)),
		(vec![PixelWidth(0), PixelHeight(1080)], (Some(0), Some(1080))),
		(vec![PixelWidth(1440)], (Some(1440), None)),
		(vec![PixelHeight(0)], (None, Some(0))),
		// A crop with default display dimensions is never read, so it is not validated.
		(
			vec![PixelWidth(1440), PixelHeight(1080), PixelCropRight(u64::MAX)],
			(Some(1440), Some(1080)),
		),
	] {
		let data = MkvBuilder::new()
			.header("matroska")
			.segment_start()
			.info(1_000_000)
			.tracks(vec![TrackEntry(Master::Full(vec![
				TrackNumber(1),
				TrackUID(1),
				TrackType(1),
				CodecID("V_VP9".into()),
				Video(Master::Full(fields)),
			]))])
			.segment_end()
			.build();
		let catalog = run(&data);
		let config = catalog.video.renditions.values().next().unwrap();
		assert_eq!((config.coded_width, config.coded_height), expected);
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(None, None)
		);
	}
}

#[test]
fn container_aspect_uncropped_display_needs_no_pixel_size() {
	use MatroskaSpec::*;
	for fields in [
		vec![DisplayWidth(16), DisplayHeight(9)],
		vec![PixelWidth(0), PixelHeight(1080), DisplayWidth(16), DisplayHeight(9)],
		vec![DisplayUnit(1), DisplayWidth(16), DisplayHeight(9)],
	] {
		let data = MkvBuilder::new()
			.header("matroska")
			.segment_start()
			.info(1_000_000)
			.tracks(vec![TrackEntry(Master::Full(vec![
				TrackNumber(1),
				TrackUID(1),
				TrackType(1),
				CodecID("V_VP9".into()),
				Video(Master::Full(fields)),
			]))])
			.segment_end()
			.build();
		let catalog = run(&data);
		let config = catalog.video.renditions.values().next().unwrap();
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(Some(16), Some(9))
		);
	}
}

#[test]
fn container_aspect_matroska_rejects_invalid_dimensions() {
	use MatroskaSpec::*;
	for fields in [
		vec![DisplayWidth(0)],
		vec![DisplayHeight(0)],
		vec![DisplayWidth(u64::MAX), DisplayHeight(1)],
		vec![DisplayUnit(5), DisplayWidth(16), DisplayHeight(9)],
		// A crop read to scale explicit display dimensions must leave a picture.
		vec![PixelCropRight(1440), DisplayWidth(16)],
		vec![PixelCropLeft(1), PixelCropRight(u64::MAX), DisplayWidth(16)],
		vec![PixelCropTop(1081), DisplayHeight(9)],
		vec![PixelCropBottom(1080), DisplayHeight(9)],
		vec![
			DisplayUnit(1),
			PixelCropRight(u64::MAX),
			DisplayWidth(16),
			DisplayHeight(9),
		],
	] {
		assert_invalid_aspect(&aspect_file("V_VP9", None, fields.clone()), &format!("{fields:?}"));
	}
	let data = aspect_file("V_VP9", None, vec![DisplayWidth(u64::MAX), DisplayHeight(u64::MAX)]);
	let catalog = run(&data);
	let config = catalog.video.renditions.values().next().unwrap();
	assert_eq!(
		(config.display_aspect_width, config.display_aspect_height),
		(Some(1), Some(1))
	);
}

#[test]
fn test_flac_catalog() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![track_entry_audio_flac(1, 48000, 2)])
		.cluster(0, || vec![simple_block(1, 0, true, b"flac-frame-0")])
		.segment_end()
		.build();

	let catalog = run(&data);
	assert_eq!(catalog.audio.renditions.len(), 1);

	let a = catalog.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Flac));
	assert_eq!(a.sample_rate, 48000);
	assert_eq!(a.channel_count, 2);
	assert!(matches!(a.container, Container::Legacy));
	// The description is the WebCodecs FLAC config: the `fLaC` marker + STREAMINFO.
	let desc = a.description.as_ref().expect("flac description");
	assert_eq!(&desc[..4], b"fLaC");
}

#[test]
fn test_vp9_only_catalog() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 1280, 720)
		.cluster(0, || vec![simple_block(1, 0, true, b"\x00\x00\x00\x01vp9-frame")])
		.segment_end()
		.build();

	let catalog = run(&data);
	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let v = catalog.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::VP9(_)), "codec: {:?}", v.codec);
	assert_eq!(v.coded_width, Some(1280));
	assert_eq!(v.coded_height, Some(720));
	assert!(matches!(v.container, Container::Legacy));
}

#[tokio::test(start_paused = true)]
async fn public_container_preserves_loc_for_mkv() {
	let payload = b"vp9-key";
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 320, 240)
		.cluster(0, || vec![simple_block(1, 0, true, payload)])
		.segment_end()
		.build();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let reserved = catalog.reserve();
	let mut import = super::Import::new(broadcast, reserved).with_container(hang::catalog::Container::Loc);
	import.decode(&data).unwrap();
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.video.renditions.iter().next().unwrap();
	assert_eq!(config.container, Container::Loc);

	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut media = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Loc(crate::container::Kind::Data),
	);
	let frame = moq_clock::timeout(std::time::Duration::from_secs(1), media.read_frame())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(frame.payload.as_ref(), payload);
}

#[test]
fn test_vp9_opus_catalog() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![
			track_entry_video_vp9(1, 640, 480),
			track_entry_audio_opus(2, 48000.0, 2),
		])
		.cluster(0, || {
			vec![
				simple_block(1, 0, true, b"vp9-key"),
				simple_block(2, 0, true, b"opus-pkt-0"),
				simple_block(2, 20, true, b"opus-pkt-1"),
				simple_block(1, 33, false, b"vp9-p"),
			]
		})
		.segment_end()
		.build();

	let catalog = run(&data);
	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 1);

	let v = catalog.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::VP9(_)));

	let a = catalog.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Opus));
	assert_eq!(a.sample_rate, 48000);
	assert_eq!(a.channel_count, 2);
}

#[test]
fn test_opus_catalog_rate_is_decoder_rate() {
	// A 44.1 kHz input rate is informational; the catalog still reports the decoder's 48 kHz.
	for header in [true, false] {
		let MatroskaSpec::TrackEntry(Master::Full(mut fields)) = track_entry_audio_opus(1, 44100.0, 2) else {
			unreachable!()
		};
		if !header {
			fields.retain(|field| !matches!(field, MatroskaSpec::CodecPrivate(_)));
		}
		let data = MkvBuilder::new()
			.header("webm")
			.segment_start()
			.info(1_000_000)
			.tracks(vec![MatroskaSpec::TrackEntry(Master::Full(fields))])
			.cluster(0, || vec![simple_block(1, 0, true, b"opus")])
			.segment_end()
			.build();

		let catalog = run(&data);
		let a = catalog.audio.renditions.values().next().unwrap();
		assert_eq!(a.sample_rate, 48000);
		assert_eq!(a.channel_count, 2);
		assert_eq!(a.description.is_some(), header);
		assert_eq!(a.warmup, Some(std::time::Duration::from_millis(80)));
	}
}

#[test]
fn test_mp3_catalog() {
	let data = MkvBuilder::new()
		.header("matroska")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![track_entry_audio_mp3(1, 44100.0, 2)])
		.cluster(0, || vec![simple_block(1, 0, true, b"mp3-frame")])
		.segment_end()
		.build();

	let catalog = run(&data);
	assert_eq!(catalog.audio.renditions.len(), 1);
	let a = catalog.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Mp3));
	assert_eq!(a.sample_rate, 44100);
	assert_eq!(a.channel_count, 2);
	assert!(a.description.is_none(), "MP3 config is in band");
}

#[test]
fn unsupported_audio_codec_is_refused() {
	// Mix of supported (Opus) and unsupported (Vorbis) audio tracks. The Vorbis track
	// must refuse the import, including when a usable Opus track precedes it.
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![
			track_entry_audio_opus(1, 48000.0, 2),
			MatroskaSpec::TrackEntry(Master::Full(vec![
				MatroskaSpec::TrackNumber(2),
				MatroskaSpec::TrackUID(2),
				MatroskaSpec::TrackType(2),
				MatroskaSpec::CodecID("A_VORBIS".to_string()),
			])),
		])
		.cluster(0, || vec![simple_block(1, 0, true, b"opus")])
		.segment_end()
		.build();

	for initial in [false, true] {
		assert!(
			matches!(import_result(&data, initial), Err(crate::Error::Mkv(super::Error::UnsupportedAudioCodec(name))) if name == "A_VORBIS")
		);
	}
}

/// A rendition must never be advertised when its media producer could not be built.
///
/// Publishing the media producer is fallible (it mints the track's HLS record track, which can
/// collide), so publishing the catalog entry first would leave consumers a rendition that is announced but has no producer behind it and is therefore
/// never served.
#[test]
fn rendition_is_not_published_when_the_media_track_fails() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![track_entry_video_vp9(1, 640, 480)])
		.segment_end()
		.build();

	// Control: the same fixture publishes exactly one rendition when nothing collides, so the
	// assertion below cannot pass merely because the fixture stopped reaching track import.
	let control = run(&data);
	assert_eq!(control.video.renditions.len(), 1, "fixture must publish a rendition");
	let name = control.video.renditions.keys().next().unwrap().clone();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

	// Squat the rendition's HLS record track, so enrolling it (and with it building its media
	// producer) fails. The handle must stay alive: the broadcast tracks names weakly, so
	// dropping it frees the name. Unique names are per broadcast, so the track is named as in
	// the control.
	let _squat = broadcast.create_track(hang::hls::default_name(&name), None).unwrap();

	let mut mkv = crate::container::mkv::Import::new(broadcast, catalog.reserve());
	let buf = bytes::BytesMut::from(&data[..]);
	// A producer failure must reach the importer caller without advertising the rendition.
	assert!(mkv.decode(&buf).is_err());

	assert!(
		catalog.snapshot().video.renditions.is_empty(),
		"a rendition whose media producer failed must not be advertised"
	);
}

/// What an MKV import published, and the wall-clock window its input arrived in.
struct Imported {
	published: std::collections::BTreeMap<String, Vec<u128>>,
	/// The root clock the catalog advertised.
	clock: hang::catalog::Clock,
	arrival: std::ops::RangeInclusive<moq_clock::Wall>,
	/// Why a chunk was refused, if one was.
	refused: Option<crate::Error>,
}

/// Import `data` in `chunk`-byte pieces on a catalog with the default clock, stopping at the
/// first refused piece.
async fn import_chunked(data: &[u8], chunk: usize) -> Imported {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut mkv = crate::container::mkv::Import::new(broadcast, catalog.reserve());

	let before = moq_clock::wall();
	let mut refused = None;
	for piece in data.chunks(chunk) {
		if let Err(err) = mkv.decode(piece) {
			refused = Some(err);
			break;
		}
	}
	let after = moq_clock::wall();
	mkv.finish().unwrap();

	let snapshot = catalog.snapshot();
	Imported {
		published: crate::container::test_util::published(&consumer, &snapshot).await,
		clock: snapshot.clock.expect("the catalog advertises a clock"),
		arrival: before..=after,
		refused,
	}
}

/// Incomplete elements span chunks, and each completed block publishes exactly once.
#[tokio::test]
async fn chunked_decode_publishes_each_block_once() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 320, 240)
		.cluster(0, || {
			vec![
				simple_block(1, 0, true, b"k0"),
				simple_block(1, 33, false, b"p1"),
				simple_block(1, 66, false, b"p2"),
			]
		})
		.cluster(100, || {
			vec![simple_block(1, 0, true, b"k1"), simple_block(1, 33, false, b"p3")]
		})
		.segment_end()
		.build();

	let import = import_chunked(&data, 16).await;
	assert!(import.refused.is_none());
	let video = import.published.values().next().unwrap();
	assert_eq!(video, &[0, 33_000, 66_000, 100_000, 133_000]);
}

/// A feed an hour into its own timeline publishes its block timestamps verbatim, and the
/// catalog clock maps its first block to the arrival time.
#[tokio::test]
async fn import_publishes_block_timestamps_on_an_arrival_clock() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 16, 16)
		.cluster(3_600_000, || {
			vec![simple_block(1, 0, true, b"k0"), simple_block(1, 33, false, b"p1")]
		})
		.segment_end()
		.build();

	let import = import_chunked(&data, data.len()).await;
	assert!(import.refused.is_none());
	let first = import.published.values().next().unwrap()[0];
	assert_eq!(first, 3_600_000_000, "the source's own timestamp");

	let tick = std::time::Duration::from_micros(1);
	let wall = import
		.clock
		.wall_clock(moq_net::Timestamp::from_micros(first as u64).unwrap())
		.unwrap();
	assert!(
		*import.arrival.start() - tick <= wall && wall <= *import.arrival.end() + tick,
		"the first block is live on arrival"
	);
}

/// A cluster that restarts the timeline is a new epoch: the import refuses the rewind rather
/// than dropping it, keeping everything published before it.
#[tokio::test]
async fn import_refuses_a_restart() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 16, 16)
		.cluster(5_000, || {
			vec![simple_block(1, 0, true, b"k0"), simple_block(1, 33, false, b"p1")]
		})
		.cluster(0, || vec![simple_block(1, 0, true, b"k1")])
		.segment_end()
		.build();

	let import = import_chunked(&data, data.len()).await;
	let err = import.refused.expect("the restart is refused");
	assert!(matches!(err, crate::Error::TimestampRewind(_)), "{err:?}");
	assert_eq!(import.published.values().next().unwrap().len(), 2);
}

/// The catalog is first published at the first block, carrying the clock that block anchors,
/// rather than at the Tracks element on a provisional clock a copy-once reader would keep.
#[tokio::test]
async fn first_catalog_carries_the_anchored_clock() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![
			track_entry_video_vp9(1, 16, 16),
			track_entry_audio_opus(2, 48_000.0, 2),
		])
		.cluster(3_600_000, || {
			vec![simple_block(2, 0, true, b"a0"), simple_block(1, 0, true, b"k0")]
		})
		.segment_end()
		.build();
	// The writer sizes the Segment at its end, so split at the Cluster's element ID.
	const CLUSTER_ID: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];
	let split = data.windows(4).position(|w| w == CLUSTER_ID).expect("a cluster");
	let (header, cluster) = data.split_at(split);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let provisional = catalog.snapshot().clock.expect("a clock");
	let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
	let mut mkv = crate::container::mkv::Import::new(broadcast, catalog.reserve());

	mkv.decode(header).unwrap();
	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1, "the VP9 rendition was read");
	let audio: Vec<_> = snapshot.audio.renditions.values().collect();
	assert!(
		matches!(audio.as_slice(), [rendition] if matches!(rendition.codec, AudioCodec::Opus)),
		"the Opus rendition was read: {audio:?}"
	);
	assert_eq!(clocks.drain(), vec![], "the Tracks element alone publishes nothing");

	mkv.decode(cluster).unwrap();
	let anchored = catalog.snapshot().clock.expect("a clock");
	assert_ne!(anchored, provisional, "the first block anchors the clock");
	let published = clocks.drain();
	assert!(!published.is_empty(), "the first block publishes the catalog");
	assert!(published.iter().all(|clock| *clock == Some(anchored)), "{published:?}");

	mkv.finish().unwrap();
	assert!(clocks.drain().iter().all(|clock| *clock == Some(anchored)));
}

/// A Tracks element after `finish()` is refused: its tracks would be declared but never finished.
#[test]
fn tracks_after_finish_are_refused() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![track_entry_audio_opus(1, 48_000.0, 2)])
		.segment_end()
		.build();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut mkv = crate::container::mkv::Import::new(broadcast, catalog.reserve());

	mkv.finish().unwrap();
	let err = mkv.decode(&data).unwrap_err();
	assert!(
		matches!(err, crate::Error::Mkv(crate::container::mkv::Error::TracksAfterFinish)),
		"{err:?}"
	);
	assert!(catalog.snapshot().audio.renditions.is_empty(), "no track was declared");
}

fn text_entry(number: u64, codec: &str, language: &str, name: &str) -> MatroskaSpec {
	MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(number),
		MatroskaSpec::TrackUID(number),
		MatroskaSpec::TrackType(17),
		MatroskaSpec::CodecID(codec.into()),
		MatroskaSpec::Language(language.into()),
		MatroskaSpec::Name(name.into()),
	]))
}

/// Opus packets packed like mkvmerge's laced audio blocks retain their boundaries and times.
#[tokio::test]
async fn laced_opus_frames() {
	use webm_iterable::matroska_spec::{BlockLacing, Frame};
	for lacing in [BlockLacing::Xiph, BlockLacing::Ebml, BlockLacing::FixedSize] {
		for grouped in [false, true] {
			let packets: Vec<&[u8]> = if matches!(lacing, BlockLacing::FixedSize) {
				vec![b"\xf8\xff\xfe", b"\xf8\xff\xfd", b"\xf8\xff\xfc"]
			} else {
				vec![b"\xf8\xff\xfe", b"\xf8\xff", b"\xf8\xff\xfc\x00"]
			};
			let mut block = SimpleBlock::new_uncheked(&[], 1, 7, false, Some(lacing), false, true);
			block.set_frame_data(&packets.iter().map(|data| Frame { data }).collect());
			let MatroskaSpec::SimpleBlock(raw) = block.into() else {
				unreachable!()
			};
			let tag = if grouped {
				MatroskaSpec::BlockGroup(Master::Full(vec![MatroskaSpec::Block(raw)]))
			} else {
				MatroskaSpec::SimpleBlock(raw)
			};
			let mut entry = track_entry_audio_opus(1, 48_000.0, 2);
			let MatroskaSpec::TrackEntry(Master::Full(children)) = &mut entry else {
				unreachable!()
			};
			children.push(MatroskaSpec::DefaultDuration(20_000_000));
			let data = MkvBuilder::new()
				.header("webm")
				.segment_start()
				.info(1_000_000)
				.tracks(vec![entry])
				.cluster(100, || vec![tag])
				.segment_end()
				.build();
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut importer = super::Import::new(broadcast, catalog.reserve());
			for piece in data.chunks(11) {
				importer.decode(piece).unwrap();
			}
			importer.finish().unwrap();
			let snapshot = catalog.snapshot();
			let (name, config) = snapshot.audio.renditions.iter().next().unwrap();
			let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
			let mut reader =
				crate::container::Consumer::new(track, crate::catalog::hang::Container::try_from(config).unwrap());
			for (index, packet) in packets.iter().enumerate() {
				let frame = reader.read_frame().await.unwrap().expect("one frame per packet");
				assert_eq!(frame.payload.as_ref(), *packet, "{lacing:?}, grouped={grouped}");
				assert_eq!(frame.timestamp.as_micros(), 107_000 + index as u128 * 20_000);
			}
			assert!(reader.read_frame().await.unwrap().is_none());
		}
	}
}

#[tokio::test]
async fn lacing_requires_default_duration() {
	use webm_iterable::matroska_spec::BlockLacing;
	for duration in [None, Some(0)] {
		let mut entry = track_entry_audio_opus(1, 48_000.0, 2);
		if let Some(duration) = duration {
			let MatroskaSpec::TrackEntry(Master::Full(children)) = &mut entry else {
				unreachable!()
			};
			children.push(MatroskaSpec::DefaultDuration(duration));
		}
		let block = SimpleBlock::new_uncheked(&[1, 0xf8, 0xf8], 1, 0, false, Some(BlockLacing::FixedSize), false, true);
		let data = MkvBuilder::new()
			.header("webm")
			.segment_start()
			.info(1_000_000)
			.tracks(vec![entry])
			.cluster(0, || vec![block.into()])
			.segment_end()
			.build();
		let imported = import_chunked(&data, data.len()).await;
		let error = imported.refused.expect("laced timing must be known").to_string();
		assert!(error.contains("lacing") && error.contains("DefaultDuration"), "{error}");
		assert!(imported.published.values().all(Vec::is_empty));
	}
}

#[tokio::test]
async fn malformed_lacing_is_refused() {
	use webm_iterable::matroska_spec::BlockLacing;
	for (lacing, payload) in [
		(BlockLacing::FixedSize, &[][..]),
		(BlockLacing::FixedSize, &[0, 0xf8][..]),
		(BlockLacing::FixedSize, &[2, 0xf8, 0xf8][..]),
		(BlockLacing::Xiph, &[1, 255][..]),
		(BlockLacing::Xiph, &[1, 10, 0xf8][..]),
		(BlockLacing::Ebml, &[2, 0x81, 0x80, 0xf8][..]), // Negative second size.
		(BlockLacing::Ebml, &[1, 0x01][..]),             // Truncated eight-byte VINT.
		(BlockLacing::Ebml, &[1, 0][..]),                // Invalid VINT marker.
	] {
		let mut entry = track_entry_audio_opus(1, 48_000.0, 2);
		let MatroskaSpec::TrackEntry(Master::Full(children)) = &mut entry else {
			unreachable!()
		};
		children.push(MatroskaSpec::DefaultDuration(20_000_000));
		let block = SimpleBlock::new_uncheked(payload, 1, 0, false, Some(lacing), false, true);
		let data = MkvBuilder::new()
			.header("webm")
			.segment_start()
			.info(1_000_000)
			.tracks(vec![entry])
			.cluster(0, || vec![block.into()])
			.segment_end()
			.build();
		let imported = import_chunked(&data, data.len()).await;
		let error = imported.refused.expect("malformed lace is refused").to_string();
		assert!(error.contains("lacing"), "{error}");
		assert!(imported.published.values().all(Vec::is_empty));
	}
}

/// An unlaced block's duration follows its cadence, not an unparsed track default.
#[tokio::test(start_paused = true)]
async fn unlaced_block_duration_does_not_use_track_default() {
	let mut entry = track_entry_video_vp9(1, 16, 16);
	let MatroskaSpec::TrackEntry(Master::Full(children)) = &mut entry else {
		unreachable!()
	};
	children.push(MatroskaSpec::DefaultDuration(20_000_000));
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![entry])
		.cluster(0, || {
			[0, 40]
				.into_iter()
				.map(|timestamp| {
					let MatroskaSpec::SimpleBlock(raw) = simple_block(1, timestamp, true, b"frame") else {
						unreachable!()
					};
					// A video Block streams once a ReferenceBlock precedes it or it ends its group.
					let mut children = vec![MatroskaSpec::BlockDuration(40)];
					if timestamp != 0 {
						children.push(MatroskaSpec::ReferenceBlock(-40));
					}
					children.push(MatroskaSpec::Block(raw));
					MatroskaSpec::BlockGroup(Master::Full(children))
				})
				.collect()
		})
		.segment_end()
		.build();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&data).unwrap();
	importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let name = snapshot.video.renditions.keys().next().unwrap();
	let mut track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut last = None;
	while let Some(mut group) = track.recv_group().await.unwrap() {
		while let Some(frame) = group.read_frame().await.unwrap() {
			let frame = hang::container::Frame::decode(frame.payload).unwrap();
			last = Some((frame.timestamp.as_micros(), frame.payload.len()));
		}
	}
	assert_eq!(last, Some((80_000, 0)));
}

/// The SPS outranks the Colour element, which still fills what the SPS leaves unsaid: here the
/// static HDR metadata.
#[test]
fn sps_color_outranks_colour_which_fills_its_gaps() {
	use crate::codec::h264::fixtures::{PPS, SPS_BT709};

	let hdr = crate::codec::color::test::hdr10();
	let mut container = crate::codec::color::cicp(9, 16, 9, Some(true)).unwrap();
	container.mastering = hdr.mastering;
	container.content_light = hdr.content_light;
	let colour = super::export::matroska_colour(&container).unwrap();

	let avcc = crate::codec::h264::build_avcc(
		&[bytes::Bytes::from_static(SPS_BT709)],
		&[bytes::Bytes::from_static(PPS)],
	)
	.unwrap();
	let entry = MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(1),
		MatroskaSpec::TrackUID(1),
		MatroskaSpec::TrackType(1),
		MatroskaSpec::CodecID("V_MPEG4/ISO/AVC".to_string()),
		MatroskaSpec::CodecPrivate(avcc.to_vec()),
		MatroskaSpec::Video(Master::Full(vec![
			MatroskaSpec::PixelWidth(64),
			MatroskaSpec::PixelHeight(64),
			colour,
		])),
	]));
	let data = MkvBuilder::new()
		.header("matroska")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![entry])
		.segment_end()
		.build();

	let catalog = run(&data);
	let color = catalog.video.renditions.values().next().unwrap().color.clone().unwrap();
	assert_eq!(
		(color.primaries, color.transfer, color.matrix, color.range),
		(Some(1), Some(1), Some(1), Some(hang::catalog::Range::Limited))
	);
	assert_eq!(
		(color.mastering, color.content_light),
		(hdr.mastering, hdr.content_light)
	);
}

/// Every catalog colour field survives a Matroska Colour element.
#[test]
fn colour_element_round_trips() {
	let hdr = crate::codec::color::test::hdr10();
	let mut color = crate::codec::color::cicp(9, 18, 9, Some(true)).unwrap();
	color.mastering = hdr.mastering;
	color.content_light = hdr.content_light;
	let MatroskaSpec::Colour(Master::Full(children)) = super::export::matroska_colour(&color).unwrap() else {
		panic!("expected a full Colour element");
	};
	assert_eq!(super::import::matroska_color(&children).unwrap(), Some(color));
	assert_eq!(super::export::matroska_colour(&hang::catalog::Color::default()), None);
}

/// A Colour value with no catalog representation is refused rather than clamped or dropped.
#[test]
fn unrepresentable_colour_is_refused() {
	for child in [
		MatroskaSpec::MaxCLL(70_000),
		MatroskaSpec::MaxFALL(65_536),
		MatroskaSpec::Primaries(300),
		MatroskaSpec::MasteringMetadata(Master::Full(vec![MatroskaSpec::LuminanceMax(f64::NAN)])),
		MatroskaSpec::MasteringMetadata(Master::Full(vec![MatroskaSpec::LuminanceMin(-1.0)])),
		MatroskaSpec::MasteringMetadata(Master::Full(vec![MatroskaSpec::PrimaryRChromaticityX(1.5)])),
	] {
		assert!(super::import::matroska_color(&[child]).is_err());
	}
}

/// Container entry points must preserve importer failures, both in the constructor's
/// initial data and in arbitrarily split subsequent chunks.
fn import_result(data: &[u8], initial: bool) -> crate::Result<()> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let init = crate::import::ContainerInit::new(
		crate::import::ContainerFormat::Mkv,
		if initial {
			bytes::Bytes::copy_from_slice(data)
		} else {
			bytes::Bytes::new()
		},
	);
	let mut import: crate::import::Container = crate::import::Container::new(broadcast, catalog.reserve(), &init)?;
	if !initial {
		for chunk in data.chunks(7) {
			import.decode(chunk)?;
		}
	}
	import.finish()
}

#[test]
fn malformed_track_metadata_fails_through_the_container() {
	use MatroskaSpec::*;
	let cases = [
		TrackEntry(Master::Full(vec![TrackType(2), CodecID("A_OPUS".into())])),
		TrackEntry(Master::Full(vec![
			TrackNumber(2),
			TrackType(2),
			CodecID("A_AAC".into()),
		])),
		TrackEntry(Master::Full(vec![
			TrackNumber(2),
			TrackType(1),
			CodecID("V_MPEG4/ISO/AVC".into()),
			CodecPrivate(vec![0]),
		])),
		TrackEntry(Master::Full(vec![
			TrackNumber(2),
			TrackType(1),
			CodecID("V_VP9".into()),
			Video(Master::Full(vec![
				PixelWidth(320),
				PixelHeight(240),
				Colour(Master::Full(vec![MaxCLL(70_000)])),
			])),
		])),
	];
	for (index, invalid) in cases.into_iter().enumerate() {
		for reverse in [false, true] {
			let mut tracks = vec![track_entry_audio_opus(1, 48_000.0, 2), invalid.clone()];
			if reverse {
				tracks.reverse();
			}
			let data = MkvBuilder::new()
				.header("matroska")
				.segment_start()
				.info(1_000_000)
				.tracks(tracks)
				.segment_end()
				.build();
			for initial in [false, true] {
				assert!(
					import_result(&data, initial).is_err(),
					"case {index}, reverse {reverse}, initial {initial}"
				);
			}
		}
	}
}

#[test]
fn duplicate_track_numbers_fail_even_for_skipped_roles() {
	use MatroskaSpec::*;
	for second in [
		track_entry_audio_opus(1, 44_100.0, 1),
		TrackEntry(Master::Full(vec![
			TrackNumber(1),
			TrackType(18),
			CodecID("B_BUTTONS".into()),
		])),
	] {
		for reverse in [false, true] {
			let mut tracks = vec![track_entry_audio_opus(1, 48_000.0, 2), second.clone()];
			if reverse {
				tracks.reverse();
			}
			let data = MkvBuilder::new()
				.header("matroska")
				.segment_start()
				.info(1_000_000)
				.tracks(tracks)
				.segment_end()
				.build();
			for initial in [false, true] {
				let result = import_result(&data, initial);
				assert!(
					matches!(&result, Err(crate::Error::Moq(moq_net::Error::Duplicate))),
					"reverse {reverse}, initial {initial}: {result:?}"
				);
			}
		}
	}
}

#[test]
fn invalid_timestamp_scales_are_refused() {
	for initial in [false, true] {
		let zero = MkvBuilder::new()
			.header("webm")
			.segment_start()
			.info(0)
			.tracks(vec![track_entry_audio_opus(1, 48_000.0, 2)])
			.cluster(0, || vec![simple_block(1, 0, true, b"opus")])
			.segment_end()
			.build();
		assert!(matches!(
			import_result(&zero, initial),
			Err(crate::Error::Mkv(super::Error::ValueOutOfRange("TimestampScale")))
		));
	}
}

#[test]
fn block_tick_overflow_is_refused_before_timestamp_conversion() {
	for (cluster, relative, scale) in [(u64::MAX, 1, 1), (u64::MAX, 0, 2)] {
		for initial in [false, true] {
			let data = MkvBuilder::new()
				.header("webm")
				.segment_start()
				.info(scale)
				.tracks(vec![track_entry_audio_opus(1, 48_000.0, 2)])
				.cluster(cluster, || vec![simple_block(1, relative, true, b"opus")])
				.segment_end()
				.build();
			let outcome = std::panic::catch_unwind(|| import_result(&data, initial));
			assert!(matches!(
				outcome,
				Ok(Err(crate::Error::Mkv(super::Error::TimestampOverflow)))
			));
		}
	}
	let negative = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1)
		.tracks(vec![track_entry_audio_opus(1, 48_000.0, 2)])
		.cluster(0, || vec![simple_block(1, -1, true, b"opus")])
		.segment_end()
		.build();
	for initial in [false, true] {
		assert!(matches!(
			import_result(&negative, initial),
			Err(crate::Error::Mkv(super::Error::NegativeBlockTimestamp))
		));
	}
}

/// A subtitle cue's WebVTT length depends on its content, so no subtitle track is advertised.
#[test]
fn subtitle_tracks_are_refused_before_their_rendition() {
	for codec in ["S_TEXT/UTF8", "S_TEXT/WEBVTT", "S_TEXT/ASS"] {
		for reverse in [false, true] {
			let mut tracks = vec![
				track_entry_audio_opus(1, 48_000.0, 2),
				text_entry(2, codec, "de", "German"),
			];
			if reverse {
				tracks.reverse();
			}
			let data = MkvBuilder::new()
				.header("matroska")
				.segment_start()
				.info(1_000_000)
				.tracks(tracks)
				.segment_end()
				.build();
			for initial in [false, true] {
				assert!(
					matches!(import_result(&data, initial), Err(crate::Error::Mkv(super::Error::SubtitleTrack(id))) if id == codec)
				);
			}
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut importer = super::Import::new(broadcast, catalog.reserve());
			assert!(importer.decode(&data).is_err());
			assert!(catalog.snapshot().text.renditions.is_empty());
		}
	}
}

#[tokio::test(start_paused = true)]
async fn tiny_chunks_preserve_tracks_and_skip_only_unsupported_roles() {
	use MatroskaSpec::*;
	let data = MkvBuilder::new()
		.header("matroska")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![
			track_entry_audio_opus(1, 48_000.0, 2),
			track_entry_audio_opus(2, 48_000.0, 1),
			TrackEntry(Master::Full(vec![
				TrackNumber(3),
				TrackType(18),
				CodecID("B_BUTTONS".into()),
				Video(Master::Full(vec![PixelWidth(u64::MAX)])),
			])),
		])
		.cluster(0, || {
			vec![
				simple_block(1, 0, true, b"stereo"),
				simple_block(2, 20, true, b"mono"),
				simple_block(3, 0, true, b"ignored"),
			]
		})
		.segment_end()
		.build();
	for chunk in [1, 2, 7, 16, data.len()] {
		let imported = import_chunked(&data, chunk).await;
		assert!(imported.refused.is_none(), "chunk {chunk}: {:?}", imported.refused);
		assert_eq!(imported.published.len(), 2, "chunk {chunk}");
		let mut timestamps: Vec<_> = imported.published.into_values().collect();
		timestamps.sort();
		assert_eq!(timestamps, [vec![0], vec![20_000]], "chunk {chunk}");
	}
}

#[test]
fn partial_track_metadata_is_refused_at_eof() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![track_entry_audio_opus(1, 48_000.0, 2)])
		.segment_end()
		.build();
	for missing in 1..=8 {
		for initial in [false, true] {
			assert!(
				matches!(
					import_result(&data[..data.len() - missing], initial),
					Err(crate::Error::Mkv(super::Error::Truncated))
				),
				"missing {missing}, initial {initial}"
			);
		}
	}
	for initial in [false, true] {
		import_result(&data, initial).unwrap();
	}
}

#[test]
fn block_ticks_above_the_signed_range_are_not_misreported_as_negative() {
	for (cluster, relative) in [(i64::MAX as u64, 1i16), (u64::MAX, -1)] {
		let data = MkvBuilder::new()
			.header("webm")
			.segment_start()
			.info(1)
			.tracks(vec![track_entry_audio_opus(1, 48_000.0, 2)])
			.cluster(cluster, || vec![simple_block(1, relative, true, b"frame")])
			.segment_end()
			.build();
		// The addition fits u64, but exceeds MoQ's existing timestamp value range.
		for initial in [false, true] {
			assert!(
				matches!(import_result(&data, initial), Err(crate::Error::TimestampOverflow(_))),
				"{cluster}+{relative}, initial {initial}"
			);
		}
	}
}

#[test]
fn invalid_track_number_fields_are_refused() {
	use MatroskaSpec::*;
	for (fields, duplicate) in [
		(vec![TrackNumber(0)], false),
		(vec![TrackNumber(1), TrackNumber(2)], true),
	] {
		let mut entry = fields;
		entry.extend([TrackType(2), CodecID("A_OPUS".into())]);
		let data = MkvBuilder::new()
			.header("webm")
			.segment_start()
			.info(1_000_000)
			.tracks(vec![TrackEntry(Master::Full(entry))])
			.segment_end()
			.build();
		for initial in [false, true] {
			let error = import_result(&data, initial).unwrap_err();
			if duplicate {
				assert!(matches!(error, crate::Error::Moq(moq_net::Error::Duplicate)));
			} else {
				assert!(matches!(
					error,
					crate::Error::Mkv(super::Error::ValueOutOfRange("TrackNumber"))
				));
			}
		}
	}
}

/// ffmpeg's live WebM uses an unknown-size Segment and real Opus packet metadata.
/// The encoded fixture needs no ffmpeg installation when this gate runs.
#[tokio::test(start_paused = true)]
async fn real_live_webm_imports_across_chunk_boundaries() {
	let data = include_bytes!("test_data/opus.webm");
	for initial in [false, true] {
		import_result(data, initial).unwrap();
	}
	for chunk in [1, 7, 64, data.len()] {
		let imported = import_chunked(data, chunk).await;
		assert!(imported.refused.is_none(), "chunk {chunk}: {:?}", imported.refused);
		assert_eq!(imported.published.len(), 1);
		let timestamps = imported.published.into_values().next().unwrap();
		assert_eq!(timestamps.len(), 6);
		assert!(timestamps.windows(2).all(|pair| pair[0] < pair[1]));
	}
}

/// A BlockGroup holding `children` around a Block for `track` with `payload`.
fn block_group(
	track: u64,
	timestamp: i16,
	payload: &[u8],
	before: Vec<MatroskaSpec>,
	after: Vec<MatroskaSpec>,
) -> MatroskaSpec {
	let MatroskaSpec::SimpleBlock(raw) = simple_block(track, timestamp, false, payload) else {
		unreachable!()
	};
	let mut children = before;
	children.push(MatroskaSpec::Block(raw));
	children.extend(after);
	MatroskaSpec::BlockGroup(Master::Full(children))
}

/// A video Block streams when what decides its group precedes it: a ReferenceBlock, or its group's
/// end. Any child after it is refused before the Block publishes, since a ReferenceBlock could follow.
#[tokio::test(start_paused = true)]
async fn video_block_groups_stream_only_when_their_group_is_known() {
	use MatroskaSpec::{BlockDuration, ReferenceBlock};
	let accepted = || {
		vec![
			block_group(1, 0, b"key", vec![], vec![]),
			block_group(1, 33, b"delta", vec![ReferenceBlock(-33)], vec![BlockDuration(33)]),
			block_group(1, 66, b"key", vec![BlockDuration(33)], vec![]),
		]
	};
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 16, 16)
		.cluster(0, accepted)
		.segment_end()
		.build();
	for chunk in [1, 5, data.len()] {
		let imported = import_chunked(&data, chunk).await;
		assert!(imported.refused.is_none(), "{:?}", imported.refused);
		assert_eq!(imported.published.values().next().unwrap(), &[0, 33_000, 66_000]);
	}

	for after in [vec![BlockDuration(33)], vec![ReferenceBlock(-33)]] {
		let data = MkvBuilder::new()
			.header("webm")
			.segment_start()
			.info(1_000_000)
			.track_video(1, "V_VP9", 16, 16)
			.cluster(0, || {
				vec![
					simple_block(1, 0, true, b"key"),
					block_group(1, 33, b"refused", vec![], after.clone()),
				]
			})
			.segment_end()
			.build();
		for chunk in [1, 5, data.len()] {
			let imported = import_chunked(&data, chunk).await;
			assert!(
				matches!(
					imported.refused,
					Some(crate::Error::Mkv(super::Error::TrailingReference))
				),
				"{:?}",
				imported.refused
			);
			assert_eq!(imported.published.values().next().unwrap(), &[0]);
		}
	}
}

/// Audio ignores a BlockGroup's trailing fields, so its Block streams whatever follows it.
#[tokio::test(start_paused = true)]
async fn audio_block_groups_stream_with_trailing_fields() {
	use MatroskaSpec::{BlockDuration, ReferenceBlock};
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![track_entry_audio_opus(1, 48_000.0, 2)])
		.cluster(0, || {
			vec![
				block_group(1, 0, b"\xf8\xff\xfe", vec![], vec![BlockDuration(20)]),
				block_group(1, 20, b"\xf8\xff\xfe", vec![], vec![ReferenceBlock(-20)]),
			]
		})
		.segment_end()
		.build();
	let imported = import_chunked(&data, 3).await;
	assert!(imported.refused.is_none(), "{:?}", imported.refused);
	assert_eq!(imported.published.values().next().unwrap(), &[0, 20_000]);
}

/// A block header's variable-length track number may split anywhere.
#[tokio::test(start_paused = true)]
async fn multibyte_track_numbers_split_anywhere() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(300, "V_VP9", 16, 16)
		.cluster(0, || {
			vec![simple_block(300, 0, true, b"k0"), simple_block(300, 33, false, b"p1")]
		})
		.segment_end()
		.build();
	for chunk in 1..=9 {
		let imported = import_chunked(&data, chunk).await;
		assert!(imported.refused.is_none(), "{:?}", imported.refused);
		assert_eq!(
			imported.published.values().next().unwrap(),
			&[0, 33_000],
			"chunk {chunk}"
		);
	}
}

/// A CMAF `moof` describes a payload before it, so MKV import refuses CMAF before any rendition.
#[test]
fn cmaf_output_is_refused_before_its_rendition() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 16, 16)
		.segment_end()
		.build();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve()).with_container(Container::Cmaf {
		init: bytes::Bytes::from_static(&[0; 32]),
	});
	assert!(matches!(
		importer.decode(&data),
		Err(crate::Error::Mkv(super::Error::StreamedCmaf))
	));
	assert!(catalog.snapshot().video.renditions.is_empty());
}

/// A seek while a block streams waits for it to end, so its frames stay in one group.
#[tokio::test(start_paused = true)]
async fn a_seek_mid_block_waits_for_the_block() {
	use webm_iterable::matroska_spec::{BlockLacing, Frame};
	let mut entry = track_entry_audio_opus(1, 48_000.0, 2);
	let MatroskaSpec::TrackEntry(Master::Full(children)) = &mut entry else {
		unreachable!()
	};
	children.push(MatroskaSpec::DefaultDuration(20_000_000));
	let packets: Vec<&[u8]> = vec![b"\xf8\xff\xfe", b"\xf8\xff\xfd"];
	let mut laced = SimpleBlock::new_uncheked(&[], 1, 0, false, Some(BlockLacing::Xiph), false, true);
	laced.set_frame_data(&packets.iter().map(|data| Frame { data }).collect());
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![entry])
		.cluster(0, || vec![laced.into(), simple_block(1, 40, true, b"\xf8\xff\xfc")])
		.segment_end()
		.build();
	// Split inside the laced block's second frame.
	let split = data.windows(3).position(|window| window == b"\xf8\xff\xfd").unwrap() + 1;

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&data[..split]).unwrap();
	importer.seek(100).unwrap();
	importer.decode(&data[split..]).unwrap();
	importer.finish().unwrap();

	let snapshot = catalog.snapshot();
	let name = snapshot.audio.renditions.keys().next().unwrap();
	let replay = moq_net::track::Subscription::default().with_max_delay(std::time::Duration::from_secs(3600));
	let mut track = consumer.track(name).unwrap().subscribe(replay).await.unwrap();
	let mut groups = Vec::new();
	while let Some(mut group) = track.recv_group().await.unwrap() {
		let mut frames = 0;
		while group.read_frame().await.unwrap().is_some() {
			frames += 1;
		}
		groups.push((group.sequence, frames));
	}
	assert_eq!(groups, [(0, 2), (100, 1)]);
}

/// A WebM with one VP9 track whose `Video` element holds `children` beside its dimensions.
fn spatial_webm(children: Vec<MatroskaSpec>) -> Vec<u8> {
	use MatroskaSpec::*;
	let mut video = vec![PixelWidth(320), PixelHeight(240)];
	video.extend(children);
	MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![TrackEntry(Master::Full(vec![
			TrackNumber(1),
			TrackType(1),
			CodecID("V_VP9".into()),
			Video(Master::Full(video)),
		]))])
		.segment_end()
		.build()
}

/// An equirectangular `Projection` whose private data is an `equi` body with these bounds.
fn equirectangular(bounds: [u32; 4], pose: [f64; 3]) -> MatroskaSpec {
	use MatroskaSpec::*;
	let mut private = vec![0; 4];
	for bound in bounds {
		private.extend(bound.to_be_bytes());
	}
	Projection(Master::Full(vec![
		ProjectionType(1),
		ProjectionPrivate(private),
		ProjectionPoseYaw(pose[0]),
		ProjectionPosePitch(pose[1]),
		ProjectionPoseRoll(pose[2]),
	]))
}

#[test]
fn projection_and_stereo_mode_fill_the_catalog() {
	use MatroskaSpec::*;
	use hang::catalog::{Projection as Proj, Stereo};
	let quarter = 1 << 30;
	for (children, projection, stereo) in [
		(vec![], None, None),
		(
			vec![StereoMode(0), equirectangular([0; 4], [0.0; 3])],
			Some(Proj::Equirectangular { fov: None }),
			None,
		),
		// VR180, side by side.
		(
			vec![StereoMode(1), equirectangular([0, 0, quarter, quarter], [0.0; 3])],
			Some(Proj::Equirectangular { fov: Some(180.0) }),
			Some(Stereo::LeftRight),
		),
		// Equirectangular without private data covers the whole sphere.
		(
			vec![StereoMode(3), Projection(Master::Full(vec![ProjectionType(1)]))],
			Some(Proj::Equirectangular { fov: None }),
			Some(Stereo::TopBottom),
		),
		(vec![Projection(Master::Full(vec![ProjectionType(0)]))], None, None),
	] {
		let catalog = run(&spatial_webm(children));
		let video = catalog.video.renditions.values().next().unwrap();
		assert_eq!((&video.projection, &video.stereo), (&projection, &stereo));
	}
}

#[test]
fn projection_and_stereo_mode_the_catalog_cannot_describe_are_refused() {
	use MatroskaSpec::*;
	let quarter = 1 << 30;
	let mut short = vec![0; 4];
	short.extend([0; 12]);
	for (name, child) in [
		("cubemap", Projection(Master::Full(vec![ProjectionType(2)]))),
		("mesh", Projection(Master::Full(vec![ProjectionType(3)]))),
		("unknown projection", Projection(Master::Full(vec![ProjectionType(4)]))),
		("asymmetric bounds", equirectangular([0, 0, quarter, 0], [0.0; 3])),
		("vertical crop", equirectangular([quarter, 0, 0, 0], [0.0; 3])),
		("yawed", equirectangular([0; 4], [90.0, 0.0, 0.0])),
		("pitched", equirectangular([0; 4], [0.0, -10.0, 0.0])),
		("rolled", equirectangular([0; 4], [0.0, 0.0, f64::NAN])),
		(
			"short private data",
			Projection(Master::Full(vec![ProjectionType(1), ProjectionPrivate(short)])),
		),
		("top-bottom right eye first", StereoMode(2)),
		("checkerboard", StereoMode(5)),
		("row interleaved", StereoMode(7)),
		("anaglyph", StereoMode(10)),
		("side by side right eye first", StereoMode(11)),
		("laced eyes", StereoMode(13)),
	] {
		let data = spatial_webm(vec![child]);
		for initial in [false, true] {
			assert!(
				matches!(
					import_result(&data, initial),
					Err(crate::Error::Mkv(super::Error::UnsupportedSpatial(_)))
				),
				"{name}"
			);
		}
	}
}

/// An OpusHead in channel mapping family 2 with `channels` channels, each its own stream.
fn ambisonic_head(channels: u8) -> Vec<u8> {
	let mut head = b"OpusHead".to_vec();
	head.extend([1, channels, 0, 0, 0x80, 0xBB, 0, 0, 0, 0, 2, channels, 0]);
	head.extend(0..channels);
	head
}

fn opus_webm(head: Vec<u8>) -> Vec<u8> {
	use MatroskaSpec::*;
	MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![TrackEntry(Master::Full(vec![
			TrackNumber(1),
			TrackType(2),
			CodecID("A_OPUS".into()),
			CodecPrivate(head),
		]))])
		.segment_end()
		.build()
}

#[test]
fn opus_family_two_is_ambisonic() {
	let catalog = run(&opus_webm(ambisonic_head(4)));
	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.layout, Some(hang::catalog::AudioLayout::Ambisonic));
	assert_eq!(audio.channel_count, 4);

	// Zeroth order, first order with a non-diegetic stereo pair, and a count RFC 8486 forbids.
	for channels in [1, 6, 5] {
		let data = opus_webm(ambisonic_head(channels));
		for initial in [false, true] {
			assert!(
				matches!(
					import_result(&data, initial),
					Err(crate::Error::Opus(crate::codec::opus::Error::UnsupportedChannelCount(c))) if c == channels as u32
				),
				"{channels} channels"
			);
		}
	}
}
