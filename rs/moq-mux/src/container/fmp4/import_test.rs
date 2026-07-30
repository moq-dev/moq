use futures::FutureExt;
use hang::catalog::Container;
use mp4_atom::{Decode, Encode};

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
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	let buf = bytes::BytesMut::from(data);
	// Ignore errors from incomplete/malformed trailing fragments in test files.
	let _ = fmp4.decode(&buf);

	catalog.snapshot()
}

fn run_fmp4_select(data: &[u8], select: crate::select::Broadcast) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

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

#[test]
fn dropping_import_retires_catalog_renditions() {
	let data = include_bytes!("test_data/bbb.mp4");
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

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
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
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
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
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
	let mut msf = crate::catalog::msf::Consumer::new(track);

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

/// Passthrough writes groups by hand (no `container::Producer`), so the timeline recorder is fed
/// directly. This guards that the import advertises the broadcast's timeline at the catalog root
/// and actually records both renditions' group opens into it, the index the VOD/playlist export
/// reads. Regression for the missing-timeline break that skipped VOD renditions.
#[tokio::test]
async fn import_populates_the_broadcast_timeline() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
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

	// The one timeline is advertised at the catalog root, named by convention.
	let section = snapshot.timeline.clone().expect("the import advertises a timeline");
	assert_eq!(section.track, hang::timeline::DEFAULT_NAME);

	// Subscribe while the producer is alive, then finish so the timeline group closes and the
	// reader terminates rather than blocking.
	let mut timeline = crate::timeline::Consumer::<()>::subscribe(&consumer, &section)
		.await
		.unwrap();
	catalog.finish().unwrap();

	// The first record indexes both renditions from their first group (sequence 0).
	let first = timeline
		.next()
		.await
		.unwrap()
		.expect("a segment should be recorded in the timeline");
	assert_eq!(first.segment, 0);
	let video_ranges = first.tracks.get(&video_name).expect("video is indexed");
	assert_eq!(video_ranges[0].start, 0, "the first video group is sequence 0");
	let audio_ranges = first.tracks.get(&audio_name).expect("audio is indexed");
	assert_eq!(audio_ranges[0].start, 0, "the first audio group is sequence 0");
}

// ---- Sample-duration handling in decode() ----

fn scale() -> moq_net::Timescale {
	moq_net::Timescale::new(1_000_000).unwrap()
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
	let frag = super::encode_fragment(1, scale(), 0, &frames).unwrap();
	let err = super::decode(frag, scale()).unwrap_err();
	assert!(matches!(err, super::Error::MissingSampleDuration), "got {err:?}");
}

/// A single-sample fragment needs no duration (nothing follows it), so it still decodes.
#[test]
fn decode_single_sample_no_duration_ok() {
	let frag = super::encode_fragment(1, scale(), 0, &[sample(0, true, None)]).unwrap();
	let out = super::decode(frag, scale()).unwrap();
	assert_eq!(out.len(), 1);
	assert_eq!(out[0].timestamp.as_micros(), 0);
}

/// With the durations the producer now backfills, every sample's DTS round-trips
/// through a multi-sample fragment.
#[test]
fn decode_multisample_with_durations_roundtrips() {
	let frames = vec![sample(0, true, Some(33_000)), sample(33_000, false, Some(33_000))];
	let frag = super::encode_fragment(1, scale(), 0, &frames).unwrap();
	let out = super::decode(frag, scale()).unwrap();
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

	let trak = super::build_audio_trak(1, 96_000, mp4_atom::Codec::from(flac));
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
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let mut video_track = consumer.track(&video_name).unwrap().subscribe(None).await.unwrap();
	let mut audio_track = consumer.track(&audio_name).unwrap().subscribe(None).await.unwrap();

	// Three 1s segments: 2 video fragments each (only the first an IDR) and 4 audio fragments,
	// so a per-fragment grouping and a per-segment one can't be confused.
	for segment in 0..3u64 {
		let base = segment * 1_000_000;
		fmp4.decode(&styp()).unwrap();

		for (i, offset) in [0u64, 500_000].into_iter().enumerate() {
			let frames = [sample(base + offset, i == 0, Some(500_000))];
			let frag = super::encode_fragment(video_id, video_scale, segment as u32, &frames).unwrap();
			fmp4.decode(&frag).unwrap();
		}
		for offset in [0u64, 250_000, 500_000, 750_000] {
			let frames = [sample(base + offset, true, Some(250_000))];
			let frag = super::encode_fragment(audio_id, audio_scale, segment as u32, &frames).unwrap();
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

/// The timeline indexes each segment as one group range per track, which is what makes the
/// segment servable: an HLS/DASH export fetches `start..=end` and concatenates it into one CMAF
/// segment. With audio grouping per fragment those ranges spanned many short groups, one FETCH
/// round-trip each, and the two tracks' boundaries no longer coincided.
#[tokio::test]
async fn segmented_source_indexes_one_group_range_per_track() {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let section = snapshot.timeline.clone().expect("the import advertises a timeline");

	// Subscribe while the producer is alive so the reader terminates on finish rather than blocking.
	let mut timeline = crate::timeline::Consumer::<()>::subscribe(&consumer, &section)
		.await
		.unwrap();

	for segment in 0..3u64 {
		let base = segment * 1_000_000;
		fmp4.decode(&styp()).unwrap();
		for (i, offset) in [0u64, 500_000].into_iter().enumerate() {
			let frag = super::encode_fragment(
				video_id,
				video_scale,
				segment as u32,
				&[sample(base + offset, i == 0, Some(500_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
		for offset in [0u64, 250_000, 500_000, 750_000] {
			let frag = super::encode_fragment(
				audio_id,
				audio_scale,
				segment as u32,
				&[sample(base + offset, true, Some(250_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
	}
	fmp4.finish().unwrap();
	catalog.finish().unwrap();

	let mut records = Vec::new();
	while let Some(record) = timeline.next().await.unwrap() {
		records.push(record);
	}
	assert_eq!(records.len(), 3, "one record per declared segment");

	for (i, record) in records.iter().enumerate() {
		assert_eq!(record.segment, i as u64);
		let video = record.tracks.get(&video_name).expect("video is indexed");
		let audio = record.tracks.get(&audio_name).expect("audio is indexed");
		assert_eq!(video.len(), 1, "segment {i}: video is one contiguous group range");
		assert_eq!(audio.len(), 1, "segment {i}: audio is one contiguous group range");
		assert_eq!(
			(video[0].start, video[0].end),
			(i as u64, i as u64),
			"segment {i}: one video group"
		);
		assert_eq!(
			(audio[0].start, audio[0].end),
			(i as u64, i as u64),
			"segment {i}: one audio group, not one per fragment"
		);
	}
}

/// Audio and video rarely start a segment at exactly the same timestamp: the boundary is nominal
/// and each track begins at its own nearest sample. The timeline files a group under a segment by
/// the group's start, so a boundary declared later than any track's first group files that whole
/// group under the previous segment.
///
/// That was survivable when audio groups were one fragment. Now a group is a whole segment, so a
/// 20ms skew moves an entire segment of audio into the wrong record: the previous segment gets a
/// second copy's worth and the next one opens with no audio at all.
///
/// `video_offset_us` shifts video relative to audio, so the same source is exercised with audio
/// leading and lagging.
async fn segment_ranges_with_skew(
	video_offset_us: i64,
) -> Vec<(Vec<hang::timeline::Range>, Vec<hang::timeline::Range>)> {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let section = snapshot.timeline.clone().unwrap();
	let mut timeline = crate::timeline::Consumer::<()>::subscribe(&consumer, &section)
		.await
		.unwrap();

	for segment in 0..3u64 {
		let base = segment * 1_000_000;
		let video_base = (base as i64 + video_offset_us).max(0) as u64;
		fmp4.decode(&styp()).unwrap();

		// Audio first, so the segment transition is detected on the audio fragment and video's
		// IDR lands after it. Two fragments each keeps the ranges unambiguous.
		for offset in [0u64, 500_000] {
			let frag = super::encode_fragment(
				audio_id,
				audio_scale,
				segment as u32,
				&[sample(base + offset, true, Some(500_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
		for (i, offset) in [0u64, 500_000].into_iter().enumerate() {
			let frag = super::encode_fragment(
				video_id,
				video_scale,
				segment as u32,
				&[sample(video_base + offset, i == 0, Some(500_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
	}
	fmp4.finish().unwrap();
	catalog.finish().unwrap();

	let mut out = Vec::new();
	while let Some(record) = timeline.next().await.unwrap() {
		out.push((
			record.tracks.get(&video_name).cloned().unwrap_or_default(),
			record.tracks.get(&audio_name).cloned().unwrap_or_default(),
		));
	}
	out
}

/// Video's IDR lands 20ms AFTER audio's first sample of the segment, the normal interleave.
#[tokio::test]
async fn audio_leading_video_still_indexes_one_group_each() {
	let records = segment_ranges_with_skew(20_000).await;
	assert_eq!(records.len(), 3, "one record per segment");
	for (i, (video, audio)) in records.iter().enumerate() {
		assert_eq!(video.len(), 1, "segment {i}: one video range, got {video:?}");
		assert_eq!(audio.len(), 1, "segment {i}: one audio range, got {audio:?}");
		assert_eq!(
			(audio[0].start, audio[0].end),
			(i as u64, i as u64),
			"segment {i} audio"
		);
		assert_eq!(
			(video[0].start, video[0].end),
			(i as u64, i as u64),
			"segment {i} video"
		);
	}
}

/// Video's IDR lands 20ms BEFORE audio's first sample, the other side of the same skew.
#[tokio::test]
async fn audio_lagging_video_still_indexes_one_group_each() {
	let records = segment_ranges_with_skew(-20_000).await;
	assert_eq!(records.len(), 3, "one record per segment");
	for (i, (video, audio)) in records.iter().enumerate() {
		assert_eq!(video.len(), 1, "segment {i}: one video range, got {video:?}");
		assert_eq!(audio.len(), 1, "segment {i}: one audio range, got {audio:?}");
		assert_eq!(
			(audio[0].start, audio[0].end),
			(i as u64, i as u64),
			"segment {i} audio"
		);
		assert_eq!(
			(video[0].start, video[0].end),
			(i as u64, i as u64),
			"segment {i} video"
		);
	}
}
