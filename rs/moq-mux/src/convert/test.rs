use std::time::Duration;

use base64::Engine;
use buf_list::BufList;
use bytes::{Bytes, BytesMut};
use hang::catalog::{Container, H264, VideoCodec, VideoConfig};
use hang::container::{Frame, Timestamp};
use mp4_atom::{Atom, DecodeMaybe};

use super::fmp4::{build_moof_mdat, build_video_init};

// ---- Helpers ----

fn ts(micros: u64) -> Timestamp {
	Timestamp::from_micros(micros).unwrap()
}

fn build_avcc_description() -> Bytes {
	let avcc = mp4_atom::Avcc {
		configuration_version: 1,
		avc_profile_indication: 0x64,
		profile_compatibility: 0x00,
		avc_level_indication: 0x1F,
		length_size: 4,
		sequence_parameter_sets: vec![vec![
			0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00,
			0x00, 0x03, 0x03, 0xC0, 0xF1, 0x62, 0xE4, 0x80,
		]],
		picture_parameter_sets: vec![vec![0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0]],
		..Default::default()
	};

	let mut buf = BytesMut::new();
	avcc.encode_body(&mut buf).expect("encode avcc");
	buf.freeze()
}

fn test_video_config() -> VideoConfig {
	VideoConfig {
		codec: VideoCodec::H264(H264 {
			profile: 0x64,
			constraints: 0x00,
			level: 0x1F,
			inline: false,
		}),
		description: Some(build_avcc_description()),
		coded_width: Some(1920),
		coded_height: Some(1080),
		framerate: Some(30.0),
		container: Container::Legacy,
		bitrate: None,
		display_ratio_width: None,
		display_ratio_height: None,
		optimize_for_latency: None,
		jitter: None,
	}
}

/// Set up an input broadcast with the given catalog config.
fn setup_input(
	video_config: &VideoConfig,
) -> (
	moq_lite::BroadcastConsumer,
	moq_lite::TrackProducer,
	moq_lite::BroadcastProducer,
	moq_lite::TrackProducer,
) {
	let mut broadcast = moq_lite::Broadcast::new().produce();

	let mut catalog_track = broadcast.create_track(hang::Catalog::default_track()).unwrap();
	let mut catalog = hang::Catalog::default();
	catalog
		.video
		.renditions
		.insert("video".to_string(), video_config.clone());

	let catalog_json = catalog.to_string().unwrap();
	let mut group = catalog_track.append_group().unwrap();
	group.write_frame(catalog_json).unwrap();
	group.finish().unwrap();

	let video_track = broadcast
		.create_track(moq_lite::Track {
			name: "video".to_string(),
			priority: 1,
		})
		.unwrap();

	let consumer = broadcast.consume();

	(consumer, video_track, broadcast, catalog_track)
}

fn write_legacy_frames(track: &mut moq_lite::TrackProducer, frames: &[(Timestamp, Vec<u8>, bool)]) {
	let mut current_group: Option<moq_lite::GroupProducer> = None;
	for (timestamp, payload, is_keyframe) in frames {
		if *is_keyframe {
			if let Some(mut g) = current_group.take() {
				g.finish().unwrap();
			}
			current_group = Some(track.append_group().unwrap());
		} else if current_group.is_none() {
			current_group = Some(track.append_group().unwrap());
		}

		let frame = Frame {
			timestamp: *timestamp,
			payload: BufList::from_iter(vec![Bytes::from(payload.clone())]),
		};
		frame.encode(current_group.as_mut().unwrap()).unwrap();
	}

	if let Some(mut g) = current_group.take() {
		g.finish().unwrap();
	}
	track.finish().unwrap();
}

fn write_cmaf_frames(track: &mut moq_lite::TrackProducer, frames: &[(u64, Vec<u8>, bool)]) {
	let mut current_group: Option<moq_lite::GroupProducer> = None;
	let mut seq: u32 = 1;
	for (dts, payload, keyframe) in frames {
		if *keyframe {
			if let Some(mut g) = current_group.take() {
				g.finish().unwrap();
			}
			current_group = Some(track.append_group().unwrap());
		} else if current_group.is_none() {
			current_group = Some(track.append_group().unwrap());
		}

		let moof_mdat = build_moof_mdat(seq, 1, *dts, payload, *keyframe).unwrap();
		seq += 1;
		current_group.as_mut().unwrap().write_frame(moof_mdat).unwrap();
	}

	if let Some(mut g) = current_group.take() {
		g.finish().unwrap();
	}
	track.finish().unwrap();
}

/// Read all Legacy frames from a track consumer (must be subscribed before converter finishes).
async fn read_legacy_frames(track: moq_lite::TrackConsumer) -> Vec<(Timestamp, Vec<u8>, bool)> {
	let mut ordered = crate::consumer::OrderedConsumer::new(track, crate::consumer::Legacy, Duration::MAX);

	let mut result = Vec::new();
	while let Some(frame) = tokio::time::timeout(Duration::from_millis(500), ordered.read())
		.await
		.expect("read_legacy_frames timed out")
		.expect("read_legacy_frames error")
	{
		let is_keyframe = frame.is_keyframe();
		let timestamp = frame.timestamp;
		let payload: Vec<u8> = frame.payload.into_iter().flat_map(|c| c.into_iter()).collect();
		result.push((timestamp, payload, is_keyframe));
	}
	result
}

/// Read all raw CMAF frames from a track consumer (must be subscribed before converter finishes).
async fn read_cmaf_raw_frames(mut track: moq_lite::TrackConsumer) -> Vec<Bytes> {
	let mut result = Vec::new();
	while let Some(group) = tokio::time::timeout(Duration::from_millis(500), track.recv_group())
		.await
		.expect("read_cmaf_raw_frames timed out")
		.expect("read_cmaf_raw_frames error")
	{
		let mut reader = group;
		while let Some(data) = tokio::time::timeout(Duration::from_millis(500), reader.read_frame())
			.await
			.expect("read_cmaf_raw_frames timed out on frame")
			.expect("read_cmaf_raw_frames frame error")
		{
			result.push(data);
		}
	}
	result
}

fn parse_cmaf_frame(data: &Bytes, timescale: u64) -> (Timestamp, Vec<u8>, bool) {
	let mut cursor = std::io::Cursor::new(data.as_ref());
	let mut moof_found = None;
	let mut mdat_found = None;

	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
		match atom {
			mp4_atom::Any::Moof(m) => moof_found = Some(m),
			mp4_atom::Any::Mdat(m) => mdat_found = Some(m),
			_ => {}
		}
	}

	let moof = moof_found.expect("no moof");
	let mdat = mdat_found.expect("no mdat");
	let traf = &moof.traf[0];
	let tfdt = traf.tfdt.as_ref().expect("no tfdt");
	let timestamp = Timestamp::from_scale(tfdt.base_media_decode_time, timescale).unwrap();
	let flags = traf.trun[0].entries[0].flags.unwrap_or(0);
	let keyframe = (flags >> 24) & 0x3 == 0x2 && (flags >> 16) & 0x1 == 0;

	(timestamp, mdat.data.clone(), keyframe)
}

/// Subscribe to the video track, retrying until it appears.
async fn subscribe_video(consumer: &moq_lite::BroadcastConsumer) -> moq_lite::TrackConsumer {
	let track = moq_lite::Track {
		name: "video".to_string(),
		priority: 1,
	};
	loop {
		match consumer.subscribe_track(&track) {
			Ok(t) => return t,
			Err(_) => tokio::task::yield_now().await,
		}
	}
}

// ---- Tests ----

#[tokio::test]
async fn legacy_to_cmaf_video() {
	let config = test_video_config();
	let frames = vec![
		(ts(0), vec![0x01, 0x02, 0x03], true),
		(ts(33_000), vec![0x04, 0x05], false),
		(ts(66_000), vec![0x06, 0x07, 0x08], true),
	];

	let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&config);
	let output = moq_lite::Broadcast::new().produce();
	let output_consumer = output.consume();

	let converter = super::Fmp4::new(consumer, output);

	let frames_clone = frames.clone();
	tokio::spawn(async move {
		tokio::task::yield_now().await;
		write_legacy_frames(&mut video_track, &frames_clone);
	});

	let (convert_result, cmaf_frames) = tokio::join!(converter.run(), async {
		let output_video = subscribe_video(&output_consumer).await;
		read_cmaf_raw_frames(output_video).await
	});
	convert_result.unwrap();

	let timescale = config.framerate.map(|f| (f * 1000.0) as u64).unwrap();
	assert_eq!(cmaf_frames.len(), 3, "expected 3 CMAF frames");

	for (i, cmaf_data) in cmaf_frames.iter().enumerate() {
		let (parsed_ts, payload, keyframe) = parse_cmaf_frame(cmaf_data, timescale);
		assert_eq!(parsed_ts, frames[i].0, "timestamp mismatch at frame {i}");
		assert_eq!(payload, frames[i].1, "payload mismatch at frame {i}");
		assert_eq!(keyframe, frames[i].2, "keyframe flag mismatch at frame {i}");
	}
}

#[tokio::test]
async fn cmaf_to_legacy_video() {
	let config = test_video_config();
	let init_data = build_video_init(&config).unwrap();
	let timescale = config.framerate.map(|f| (f * 1000.0) as u64).unwrap();

	let cmaf_frames: Vec<(u64, Vec<u8>, bool)> = vec![
		(0, vec![0x01, 0x02, 0x03], true),
		(33_000u64 * timescale / 1_000_000, vec![0x04, 0x05], false),
		(66_000u64 * timescale / 1_000_000, vec![0x06, 0x07, 0x08], true),
	];

	let mut cmaf_config = config.clone();
	cmaf_config.container = Container::Cmaf {
		init_data: base64::engine::general_purpose::STANDARD.encode(&init_data),
	};

	let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&cmaf_config);
	let output = moq_lite::Broadcast::new().produce();
	let output_consumer = output.consume();
	let converter = super::Hang::new(consumer, output);

	let cmaf_frames_clone = cmaf_frames.clone();
	tokio::spawn(async move {
		tokio::task::yield_now().await;
		write_cmaf_frames(&mut video_track, &cmaf_frames_clone);
	});

	let (convert_result, legacy_frames) = tokio::join!(converter.run(), async {
		let output_video = subscribe_video(&output_consumer).await;
		read_legacy_frames(output_video).await
	});
	convert_result.unwrap();

	assert_eq!(legacy_frames.len(), 3, "expected 3 Legacy frames");
	assert_eq!(legacy_frames[0].0, ts(0));
	assert_eq!(legacy_frames[0].1, vec![0x01, 0x02, 0x03]);
	assert_eq!(legacy_frames[1].0, ts(33_000));
	assert_eq!(legacy_frames[1].1, vec![0x04, 0x05]);
	assert_eq!(legacy_frames[2].0, ts(66_000));
	assert_eq!(legacy_frames[2].1, vec![0x06, 0x07, 0x08]);
}

#[tokio::test]
async fn roundtrip_legacy_cmaf_legacy() {
	let config = test_video_config();
	let frames = vec![
		(ts(0), vec![0xAA, 0xBB], true),
		(ts(33_000), vec![0xCC], false),
		(ts(66_000), vec![0xDD, 0xEE, 0xFF], true),
		(ts(99_000), vec![0x11, 0x22], false),
	];

	let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&config);

	// Legacy → CMAF
	let cmaf_output = moq_lite::Broadcast::new().produce();
	let cmaf_consumer = cmaf_output.consume();
	let fmp4_converter = super::Fmp4::new(consumer, cmaf_output);

	// CMAF → Legacy
	let legacy_output = moq_lite::Broadcast::new().produce();
	let legacy_consumer = legacy_output.consume();
	let hang_converter = super::Hang::new(cmaf_consumer, legacy_output);

	let frames_clone = frames.clone();
	tokio::spawn(async move {
		tokio::task::yield_now().await;
		write_legacy_frames(&mut video_track, &frames_clone);
	});

	let (r1, r2, result) = tokio::join!(fmp4_converter.run(), hang_converter.run(), async {
		let legacy_video = subscribe_video(&legacy_consumer).await;
		read_legacy_frames(legacy_video).await
	});
	r1.unwrap();
	r2.unwrap();

	assert_eq!(result.len(), frames.len(), "frame count mismatch after roundtrip");

	for (i, (expected_ts, expected_payload, _)) in frames.iter().enumerate() {
		assert_eq!(result[i].0, *expected_ts, "timestamp mismatch at frame {i}");
		assert_eq!(result[i].1, *expected_payload, "payload mismatch at frame {i}");
	}
}

#[tokio::test]
async fn cmaf_passthrough() {
	let config = test_video_config();
	let init_data = build_video_init(&config).unwrap();
	let timescale = config.framerate.map(|f| (f * 1000.0) as u64).unwrap();

	let cmaf_frames: Vec<(u64, Vec<u8>, bool)> = vec![
		(0, vec![0x01, 0x02], true),
		(33_000u64 * timescale / 1_000_000, vec![0x03, 0x04], false),
	];

	let mut cmaf_config = config.clone();
	cmaf_config.container = Container::Cmaf {
		init_data: base64::engine::general_purpose::STANDARD.encode(&init_data),
	};

	let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&cmaf_config);
	let output = moq_lite::Broadcast::new().produce();
	let output_consumer = output.consume();

	let converter = super::Fmp4::new(consumer, output);

	let cmaf_frames_clone = cmaf_frames.clone();
	tokio::spawn(async move {
		tokio::task::yield_now().await;
		write_cmaf_frames(&mut video_track, &cmaf_frames_clone);
	});

	let (convert_result, output_frames) = tokio::join!(converter.run(), async {
		let output_video = subscribe_video(&output_consumer).await;
		read_cmaf_raw_frames(output_video).await
	});
	convert_result.unwrap();

	assert_eq!(output_frames.len(), cmaf_frames.len());

	let mut seq = 1u32;
	for (i, (dts, payload, keyframe)) in cmaf_frames.iter().enumerate() {
		let expected = build_moof_mdat(seq, 1, *dts, payload, *keyframe).unwrap();
		seq += 1;
		assert_eq!(output_frames[i], expected, "frame {i} should be byte-identical");
	}
}

#[tokio::test]
async fn legacy_passthrough() {
	let config = test_video_config();
	let frames = vec![(ts(0), vec![0xAA, 0xBB], true), (ts(33_000), vec![0xCC, 0xDD], false)];

	let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&config);
	let output = moq_lite::Broadcast::new().produce();
	let output_consumer = output.consume();

	let converter = super::Hang::new(consumer, output);

	let frames_clone = frames.clone();
	tokio::spawn(async move {
		tokio::task::yield_now().await;
		write_legacy_frames(&mut video_track, &frames_clone);
	});

	let (convert_result, result) = tokio::join!(converter.run(), async {
		let output_video = subscribe_video(&output_consumer).await;
		read_legacy_frames(output_video).await
	});
	convert_result.expect("converter.run() failed");

	assert_eq!(result.len(), frames.len());

	for (i, (expected_ts, expected_payload, _)) in frames.iter().enumerate() {
		assert_eq!(result[i].0, *expected_ts, "timestamp mismatch at frame {i}");
		assert_eq!(result[i].1, *expected_payload, "payload mismatch at frame {i}");
	}
}
