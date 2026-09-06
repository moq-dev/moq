//! Tests for the MPEG-TS exporter.
//!
//! AAC audio is framed as ADTS (MP2/AC-3 pass through as whole frames); video
//! is normalized to length-prefixed NALU by
//! `ExportSource` and rewritten to Annex-B by the muxer (re-injecting the
//! parameter sets on keyframes). These build a synthetic broadcast, export to
//! TS, and re-parse with the `mpeg2ts` reader.

use std::io::Cursor;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, VideoConfig};
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::{ReadTsPacket, TsPacketReader, TsPayload};

use crate::catalog::hang::Container as HangContainer;
use crate::container::ts::export::PCR_INTERVAL;
use crate::container::ts::{Export, catalog as tscat};
use crate::container::{Frame, Producer};
use moq_net::Timestamp;

const SC: &[u8] = &[0, 0, 0, 1];
// Reusable H.264 parameter-set and slice NALs (NAL type = first byte & 0x1f).
const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xde];
const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
// A second, distinct PPS (id 1): broadcast feeds often define more than one.
const PPS1: &[u8] = &[0x68, 0xce, 0x3c, 0x81];

// libklvanc public-sample SCTE-35 cue: splice_info_section, table_id 0xFC, 30 bytes.
const CUE: &[u8] = &[
	0xfc, 0x30, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf0, 0x0a, 0x05, 0x00, 0x00, 0x2b, 0xb4, 0x7f,
	0xdf, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xad, 0x25, 0xe8, 0x39,
];

/// Concatenate NALs into an Annex-B buffer (4-byte start code before each).
fn annexb(nals: &[&[u8]]) -> Bytes {
	let mut buf = BytesMut::new();
	for nal in nals {
		buf.extend_from_slice(SC);
		buf.extend_from_slice(nal);
	}
	buf.freeze()
}

/// Concatenate NALs into a length-prefixed (avc1/hvc1) buffer (4-byte big-endian
/// length before each), the wire shape of an out-of-band source.
fn length_prefixed(nals: &[&[u8]]) -> Bytes {
	let mut buf = BytesMut::new();
	for nal in nals {
		buf.extend_from_slice(&(nal.len() as u32).to_be_bytes());
		buf.extend_from_slice(nal);
	}
	buf.freeze()
}

/// Drive an exporter until it stops producing output, concatenating every chunk.
///
/// The broadcast producers stay alive so the exporter can subscribe to the
/// finished, retained tracks; that means it never reaches a hard end-of-stream,
/// so we pull until a `next()` blocks (`Pending`, surfaced as a timeout under
/// paused time) or the stream ends.
async fn drain(consumer: moq_net::broadcast::Consumer) -> BytesMut {
	drain_with(Export::new(crate::source::announced(&consumer)).await.unwrap()).await
}

/// `drain` for an exporter built with an explicit catalog extension.
async fn drain_with<E: tscat::Catalog>(mut exporter: Export<E>) -> BytesMut {
	let mut out = BytesMut::new();
	// `while let Ok` stops on the first timeout (`Pending`: no more output).
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await {
		let Some(frame) = res.expect("exporter error") else {
			break;
		};
		out.extend_from_slice(&frame.payload);
	}
	out
}

/// An adaptation-field-only single-packet frame: the exporter's PCR carriage.
fn is_pcr_frame(frame: &Frame) -> bool {
	frame.payload.len() == 188 && frame.payload[3] & 0x30 == 0x20
}

fn assert_packet_aligned(ts: &[u8]) {
	assert!(!ts.is_empty(), "no TS output");
	assert_eq!(ts.len() % 188, 0, "output not a whole number of 188-byte packets");
	assert!(
		ts.chunks(188).all(|p| p[0] == 0x47),
		"every packet must start with the sync byte"
	);
}

#[tokio::test(start_paused = true)]
async fn export_aac_roundtrip() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".aac"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut cfg = AudioConfig::new(AAC { profile: 2 }, 48_000, 2);
		cfg.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(name.clone(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	// The last frame is > 184 bytes to force PES splitting across TS packets.
	let frames: Vec<Bytes> = vec![
		Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]),
		Bytes::from_static(&[0x10, 0x11, 0x12, 0x13, 0x14]),
		Bytes::from(vec![0x20u8; 200]),
	];
	for (i, payload) in frames.iter().enumerate() {
		producer
			.write(Frame {
				timestamp: Timestamp::from_micros(i as u64 * 20_000).unwrap(),
				duration: None,
				payload: payload.clone(),
				keyframe: true,
			})
			.unwrap();
		producer.cut(None).unwrap();
	}
	producer.finish().unwrap();

	// The producers stay alive so the exporter can subscribe to the catalog and
	// the finished (retained) track; `drain` stops once all frames are emitted.
	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	// Pass 1: the program tables advertise exactly one ADTS AAC stream.
	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut saw_pat = false;
	let mut saw_pmt = false;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		match packet.payload {
			Some(TsPayload::Pat(_)) => saw_pat = true,
			Some(TsPayload::Pmt(pmt)) => {
				saw_pmt = true;
				assert_eq!(pmt.es_info.len(), 1);
				assert_eq!(pmt.es_info[0].stream_type, StreamType::AdtsAac);
			}
			_ => {}
		}
	}
	assert!(saw_pat, "missing PAT");
	assert!(saw_pmt, "missing PMT");

	// Pass 2: reassemble PES packets and recover the original raw AAC frames.
	let mut pes = PesPacketReader::new(TsPacketReader::new(Cursor::new(ts.as_ref())));
	let mut recovered: Vec<(u64, Vec<u8>)> = Vec::new();
	while let Some(packet) = pes.read_pes_packet().unwrap() {
		let pts = packet.header.pts.expect("PES carried no PTS").as_u64();
		// Strip the 7-byte ADTS header we added on export.
		assert!(packet.data.len() >= 7, "PES payload shorter than an ADTS header");
		recovered.push((pts, packet.data[7..].to_vec()));
	}

	assert_eq!(recovered.len(), frames.len());
	for (i, payload) in frames.iter().enumerate() {
		let (pts, raw) = &recovered[i];
		assert_eq!(*pts, i as u64 * 20 * 90, "PTS should be ms * 90 (90 kHz)");
		assert_eq!(raw.as_slice(), payload.as_ref(), "raw AAC payload mismatch");
	}
}

/// Collect PES presentation timestamps per elementary stream (video H.264, audio AAC),
/// keyed off the PMT's PID assignments.
fn collect_pes_pts(ts: &[u8]) -> (Vec<u64>, Vec<u64>) {
	let mut reader = TsPacketReader::new(Cursor::new(ts));
	let (mut video_pid, mut audio_pid) = (None, None);
	let (mut video, mut audio) = (Vec::new(), Vec::new());
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		match packet.payload {
			Some(TsPayload::Pmt(pmt)) => {
				for es in &pmt.es_info {
					match es.stream_type {
						StreamType::H264 => video_pid = Some(es.elementary_pid),
						StreamType::AdtsAac => audio_pid = Some(es.elementary_pid),
						_ => {}
					}
				}
			}
			Some(TsPayload::PesStart(pes)) => {
				if let Some(pts) = pes.header.pts {
					let pid = Some(packet.header.pid);
					if pid == video_pid {
						video.push(pts.as_u64());
					} else if pid == audio_pid {
						audio.push(pts.as_u64());
					}
				}
			}
			_ => {}
		}
	}
	(video, audio)
}

/// Build a broadcast whose audio begins before the first video keyframe (the shape a
/// mid-stream tune-in produces: the audio source is cached further back than the oldest
/// retained video keyframe), then export it to TS.
async fn export_lead_audio() -> BytesMut {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	// In-band avc3 video (SPS/PPS inline on keyframes; no out-of-band description).
	let vtrack = broadcast
		.create_track(broadcast.unique_name(".avc3"), hang::container::track_info())
		.unwrap();
	{
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		cfg.container = Container::Legacy;
		catalog.lock().video.renditions.insert(vtrack.name().to_string(), cfg);
	}
	let mut video = Producer::new(vtrack, HangContainer::Legacy);

	let atrack = broadcast
		.create_track(broadcast.unique_name(".aac"), hang::container::track_info())
		.unwrap();
	{
		let mut cfg = AudioConfig::new(AAC { profile: 2 }, 48_000, 2);
		cfg.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(atrack.name().to_string(), cfg);
	}
	let mut audio = Producer::new(atrack, HangContainer::Legacy);

	let audio_frame = |ms: u64| Frame {
		timestamp: Timestamp::from_micros(ms * 1_000).unwrap(),
		duration: None,
		payload: Bytes::from(vec![0xAAu8; 16]),
		keyframe: true,
	};
	// Lead audio (0..80 ms) precedes the first video keyframe at 100 ms; both continue after.
	for ms in [0, 20, 40, 60, 80] {
		audio.write(audio_frame(ms)).unwrap();
		audio.cut(None).unwrap();
	}
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 200));
	video
		.write(Frame {
			timestamp: Timestamp::from_micros(100_000).unwrap(),
			duration: None,
			payload: annexb(&[SPS, PPS, &idr]),
			keyframe: true,
		})
		.unwrap();
	video.cut(None).unwrap();
	for ms in [100, 120, 140] {
		audio.write(audio_frame(ms)).unwrap();
		audio.cut(None).unwrap();
	}
	video.finish().unwrap();
	audio.finish().unwrap();

	let exporter = Export::new(crate::source::announced(&consumer)).await.unwrap();
	// The producers stay alive through the drain so the retained tracks are readable.
	drain_with(exporter).await
}

/// The exported stream must begin at the first video keyframe. On a mid-stream tune-in the
/// audio source can lead the first cached video keyframe by over a second; emitting that
/// audio first buries the in-band SPS/PPS behind an audio-only preamble, and a live decoder
/// probing the stream gives up before it ever configures video (RTMP/CMAF carry the codec
/// config out-of-band, so they don't hit this). The muxer drops the lead audio so the
/// keyframe leads. Audio from the keyframe onward is still carried.
#[tokio::test(start_paused = true)]
async fn export_starts_at_video_keyframe() {
	// 100 ms (the keyframe PTS) in 90 kHz ticks.
	const KEYFRAME_PTS: u64 = 100 * 90;

	let ts = export_lead_audio().await;
	assert_packet_aligned(&ts);
	let (video, audio) = collect_pes_pts(&ts);

	assert_eq!(
		video.first(),
		Some(&KEYFRAME_PTS),
		"the stream must begin at the video keyframe"
	);
	assert!(
		audio.iter().all(|&p| p >= KEYFRAME_PTS),
		"lead audio before the first keyframe must be dropped, got {audio:?}"
	);
	assert!(!audio.is_empty(), "audio from the keyframe onward is still carried");
}

/// Re-parse a TS byte stream: assert the single video stream type, that the
/// keyframe carries random-access + PCR in an unbounded PES, and return the
/// reassembled Annex-B elementary stream.
fn reassemble_video(ts: &[u8], expected_stream_type: StreamType) -> Vec<u8> {
	let mut reader = TsPacketReader::new(Cursor::new(ts));
	let mut video_pid = None;
	let mut saw_random_access = false;
	let mut saw_pcr = false;
	let mut reassembled: Vec<u8> = Vec::new();
	let mut unbounded = false;

	while let Some(packet) = reader.read_ts_packet().unwrap() {
		match packet.payload {
			Some(TsPayload::Pmt(pmt)) => {
				assert_eq!(pmt.es_info.len(), 1);
				assert_eq!(pmt.es_info[0].stream_type, expected_stream_type);
				video_pid = Some(pmt.es_info[0].elementary_pid);
			}
			Some(TsPayload::PesStart(pes)) => {
				// The first packet of a keyframe must signal random access.
				if let Some(af) = &packet.adaptation_field {
					saw_random_access |= af.random_access_indicator;
				}
				unbounded = pes.pes_packet_len == 0;
				reassembled.extend_from_slice(&pes.data);
			}
			Some(TsPayload::PesContinuation(bytes)) => reassembled.extend_from_slice(&bytes),
			// The clock rides adaptation-field-only packets on the PCR PID.
			None => saw_pcr |= packet.adaptation_field.as_ref().is_some_and(|af| af.pcr.is_some()),
			_ => {}
		}
	}

	assert!(video_pid.is_some(), "missing video PMT entry");
	assert!(saw_random_access, "keyframe should set random_access_indicator");
	assert!(saw_pcr, "PCR pid should carry the clock");
	assert!(unbounded, "video PES should be unbounded");
	reassembled
}

/// In-band avc3: SPS/PPS are inline in the bitstream. ExportSource strips them
/// into a synthesized avcC, and the muxer re-injects them on the keyframe.
#[tokio::test(start_paused = true)]
async fn export_avc3_in_band_reassembles() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".avc3"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: true,
		});
		cfg.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name.clone(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	// IDR slice (NAL type 5), padded past 184 bytes to span multiple TS packets.
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 300));
	// Annex-B keyframe: inline SPS + PPS + IDR.
	producer
		.write(Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			duration: None,
			payload: annexb(&[SPS, PPS, &idr]),
			keyframe: true,
		})
		.unwrap();
	producer.finish().unwrap();

	// Keep the producers alive (see `export_aac_roundtrip`).
	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let reassembled = reassemble_video(&ts, StreamType::H264);
	// The parameter sets the muxer re-injected, followed by the slice, all Annex-B.
	assert_eq!(reassembled.as_slice(), annexb(&[SPS, PPS, &idr]).as_ref());
}

/// In-band avc3 carrying two distinct PPS (a real broadcast trait): both must
/// survive the round-trip, or slices referencing the dropped one stop decoding
/// (regression for non-existing PPS 0 referenced).
#[tokio::test(start_paused = true)]
async fn export_avc3_preserves_multiple_pps() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".avc3"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: true,
		});
		cfg.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name.clone(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 300));
	// Annex-B keyframe: inline SPS + both PPS + IDR.
	producer
		.write(Frame {
			timestamp: Timestamp::from_millis(0).unwrap(),
			duration: None,
			payload: annexb(&[SPS, PPS, PPS1, &idr]),
			keyframe: true,
		})
		.unwrap();
	producer.finish().unwrap();

	// Keep the producers alive (see `export_aac_roundtrip`).
	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let reassembled = reassemble_video(&ts, StreamType::H264);
	// Both PPS must be re-injected on the keyframe, in order, ahead of the slice.
	assert_eq!(reassembled.as_slice(), annexb(&[SPS, PPS, PPS1, &idr]).as_ref());
}

/// Out-of-band avc1 (e.g. from fmp4 import): length-prefixed NALs with the
/// SPS/PPS only in the catalog `description` (avcC). The muxer must parse the
/// avcC, prepend the parameter sets as Annex-B on the keyframe, and rewrite the
/// length prefixes to start codes.
#[tokio::test(start_paused = true)]
async fn export_avc1_out_of_band_reassembles() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".avc1"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		cfg.container = Container::Legacy;
		cfg.description = Some(avcc);
		catalog.lock().video.renditions.insert(name.clone(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	// IDR slice (NAL type 5), padded past 184 bytes to span multiple TS packets.
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 300));
	// Length-prefixed keyframe: just the slice, no inline parameter sets.
	producer
		.write(Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			duration: None,
			payload: length_prefixed(&[&idr]),
			keyframe: true,
		})
		.unwrap();
	producer.finish().unwrap();

	// Keep the producers alive (see `export_aac_roundtrip`).
	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let reassembled = reassemble_video(&ts, StreamType::H264);
	// SPS/PPS from the avcC must precede the slice, all converted to Annex-B.
	assert_eq!(reassembled.as_slice(), annexb(&[SPS, PPS, &idr]).as_ref());
}

/// H.265 suffix SEI must survive the container seam in both directions: the exporter
/// puts each access unit on the wire whole, and the importer's splitter gives it back
/// unchanged. A splitter that closed the access unit on a suffix SEI would hand it to
/// the next picture, and drop the last one entirely when the stream ends, which a unit
/// test on the splitter alone cannot see once the bytes cross a PES boundary.
#[tokio::test(start_paused = true)]
async fn export_import_h265_keeps_suffix_sei_on_its_picture() {
	use crate::codec::h265::fixtures::{PPS, SPS, VPS};

	// HEVC NAL headers: byte 0 = nal_unit_type << 1. Slices set
	// first_slice_segment_in_pic_flag (byte 2 high bit).
	const IDR: &[u8] = &[0x26, 0x01, 0x80, 0xaa]; // IdrWRadl (19)
	const TRAIL: &[u8] = &[0x02, 0x01, 0x80, 0x33]; // TrailR (1)
	const AUD: &[u8] = &[0x46, 0x01, 0x50]; // AudNut (35)
	const PREFIX_SEI: &[u8] = &[0x4e, 0x01, 0x01, 0x04, 0x80]; // PrefixSeiNut (39)
	const SUFFIX_SEI: &[u8] = &[0x50, 0x01, 0x84, 0x02, 0x80]; // SuffixSeiNut (40)
	const SUFFIX_SEI2: &[u8] = &[0x50, 0x01, 0x05, 0x03, 0x80]; // a second SuffixSeiNut

	// Three access units covering every shape: multiple suffix units, a prefix SEI
	// immediately after a suffix, consecutive pictures, and a suffix at end of stream.
	let units: [Vec<&[u8]>; 3] = [
		vec![VPS, SPS, PPS, IDR, SUFFIX_SEI, SUFFIX_SEI2],
		vec![PREFIX_SEI, TRAIL, SUFFIX_SEI],
		vec![AUD, TRAIL, SUFFIX_SEI],
	];

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".hev1"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		// hev1: the config comes from the SPS the keyframe carries inline.
		let mut cfg = crate::codec::h265::config(&annexb(&units[0])).unwrap();
		cfg.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name.clone(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	for (i, nals) in units.iter().enumerate() {
		producer
			.write(Frame {
				timestamp: Timestamp::from_millis(i as u64 * 40).unwrap(),
				duration: None,
				payload: annexb(nals),
				keyframe: i == 0,
			})
			.unwrap();
	}
	producer.finish().unwrap();

	// Keep the producers alive (see `export_aac_roundtrip`).
	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	// Export: the elementary stream is the three access units back to back, every SEI
	// still trailing the picture it was written with.
	let all: Vec<&[u8]> = units.iter().flatten().copied().collect();
	let reassembled = reassemble_video(&ts, StreamType::H265);
	assert_eq!(reassembled.as_slice(), annexb(&all).as_ref());

	// Import: the same TS back through the demuxer must rebuild the same access units.
	let mut imported = moq_net::broadcast::Info::new().produce();
	let imported_consumer = imported.consume();
	let import_catalog = crate::catalog::Producer::new(&mut imported).unwrap();
	let mut import = crate::container::ts::Import::new(imported, import_catalog.reserve());
	import.decode(&ts).unwrap();
	import.finish().unwrap();

	let snapshot = import_catalog.snapshot();
	let (imported_name, video) = snapshot.video.renditions.iter().next().expect("an H.265 rendition");
	assert!(video.codec.to_string().starts_with("hev1"), "codec was {}", video.codec);

	let recovered = read_frames(&imported_consumer, imported_name).await;
	assert_eq!(recovered.len(), units.len(), "access unit count");
	for (i, (got, nals)) in recovered.iter().zip(&units).enumerate() {
		assert_eq!(got.as_slice(), annexb(nals).as_ref(), "access unit {i}");
	}
}

/// A real broadcast contribution feed (Ateme Kyrion, H.264 1080i with ~86 B-frames)
/// must come out of the exporter with an authored decode timeline. The importer publishes
/// the reorder depth as the catalog `jitter`, and the exporter sizes its decode-clock reserve
/// from it, so the video PES carry a DTS that is both strictly increasing and never after the
/// PTS in decode order. Also assert the reorder was real (non-monotonic PTS in the source).
#[tokio::test(start_paused = true)]
async fn export_bframe_video_authors_dts() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	// `import` and `catalog` stay alive: retained tracks the exporter subscribes to.
	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	// Collect (pts, dts) for the H.264 video PID in transport (decode) order.
	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut video_pid = None;
	let mut pts = Vec::new();
	let mut authored = 0usize;
	let mut effective = Vec::new();
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		match packet.payload {
			Some(TsPayload::Pmt(pmt)) if video_pid.is_none() => {
				video_pid = pmt
					.es_info
					.iter()
					.find(|e| e.stream_type == StreamType::H264)
					.map(|e| e.elementary_pid);
			}
			Some(TsPayload::PesStart(pes)) if Some(packet.header.pid) == video_pid => {
				let p = pes.header.pts.expect("video PES carried no PTS").as_u64();
				let d = pes.header.dts.map(|t| t.as_u64());
				if d.is_some() {
					authored += 1;
				}
				effective.push(d.unwrap_or(p));
				pts.push(p);
			}
			_ => {}
		}
	}

	assert!(video_pid.is_some(), "missing H.264 video PMT entry");
	assert!(pts.len() > 50, "expected the full feed, got {} frames", pts.len());
	// The source is genuinely reordered: PTS dips in decode order (B-frames).
	assert!(
		pts.windows(2).any(|w| w[1] < w[0]),
		"fixture must carry reordered B-frames"
	);
	// The exporter authored a decode timeline (the decode clock trails the PTS).
	assert!(authored > 0, "no DTS authored for a B-frame stream");
	// Strictly increasing (removes the `+igndts` requirement) and never after presentation
	// (the catalog jitter sized the reserve to the reorder depth).
	for (i, win) in effective.windows(2).enumerate() {
		assert!(win[1] > win[0], "DTS not strictly increasing at frame {i}: {win:?}");
	}
	for (i, (&d, &p)) in effective.iter().zip(pts.iter()).enumerate() {
		assert!(d <= p, "DTS {d} after PTS {p} at frame {i}");
	}
}

/// #2937: the PCR must be a uniform bounded-interval ramp, not a sample of the
/// per-unit decode clock. On a reordered (B-frame) capture the authored DTS is a
/// saw (reference frames leap a reorder span, B-frames nudge one tick), so a PCR
/// taken from it froze and jumped: most intervals landed within microseconds of
/// each other, the rest collected into gaps far over TR 101 290's 40 ms gate.
#[tokio::test(start_paused = true)]
async fn export_pcr_is_a_uniform_ramp() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	// Walk the output in transport order: PCR values (90 kHz) as they appear, and
	// every PES unit's effective decode time against the clock preceding it.
	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut pcr_pid = None;
	let mut pcr_pids = Vec::new();
	let mut pcrs: Vec<u64> = Vec::new();
	let mut units = 0usize;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(pcr) = packet.adaptation_field.as_ref().and_then(|af| af.pcr) {
			// The clock leads the stream, so the first PCR precedes the PMT that
			// names its PID; collect and check at the end.
			pcr_pids.push(packet.header.pid);
			assert!(packet.payload.is_none(), "PCR rides adaptation-field-only packets");
			pcrs.push(pcr.as_u64() / 300);
		}
		match packet.payload {
			Some(TsPayload::Pmt(pmt)) if pcr_pid.is_none() => pcr_pid = pmt.pcr_pid,
			Some(TsPayload::PesStart(pes)) => {
				units += 1;
				let pts = pes.header.pts.expect("PES carried no PTS").as_u64();
				let decode = pes.header.dts.map(|t| t.as_u64()).unwrap_or(pts);
				if let Some(&pcr) = pcrs.last() {
					assert!(decode >= pcr, "unit decodes at {decode}, before the clock at {pcr}");
				}
			}
			_ => {}
		}
	}

	assert!(units > 50, "expected the full feed, got {units} PES units");
	assert!(pcrs.len() > 50, "expected a dense clock, got {} PCRs", pcrs.len());
	let pcr_pid = pcr_pid.expect("PMT must announce a PCR PID");
	assert!(
		pcr_pids.iter().all(|&pid| pid == pcr_pid),
		"PCR must ride the announced PID"
	);
	// One grid step apart, exactly: uniform, monotonic, and far under the 40 ms gate.
	let step = Duration::from_millis(25).as_micros() as u64 * 90 / 1_000;
	for (i, w) in pcrs.windows(2).enumerate() {
		assert_eq!(w[1] - w[0], step, "PCR interval off the grid at {i}: {w:?}");
	}
}

/// A timeline that starts inside the decode-clock reserve backs the PCR off
/// through the 33-bit wrap instead of saturating at zero: the grid step stays
/// uniform from the very first slot (saturation would emit 0 then 2234, and a
/// large catalog jitter would freeze several leading PCRs at zero).
#[tokio::test(start_paused = true)]
async fn export_pcr_wraps_below_the_reserve_at_start() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".aac"), hang::container::track_info())
		.unwrap();
	{
		let mut cfg = AudioConfig::new(AAC { profile: 2 }, 48_000, 2);
		cfg.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(track.name().to_string(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);
	for i in 0..4u64 {
		producer
			.write(Frame {
				timestamp: Timestamp::from_micros(i * 20_000).unwrap(),
				duration: None,
				payload: Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]),
				keyframe: true,
			})
			.unwrap();
	}
	producer.finish().unwrap();

	let mut exporter = Export::new(crate::source::announced(&consumer)).await.unwrap();
	let frames = drain_frames(&mut exporter).await;

	// A slot's clock packets lead the frame carrying that slot's bytes, and the
	// frame is stamped at the slot boundary so the caller's pacer delivers the
	// clock at the time it asserts.
	let mut pcrs: Vec<u64> = Vec::new();
	for (i, frame) in frames.iter().enumerate() {
		assert_packet_aligned(&frame.payload);
		let mut head = None;
		for packet in frame.payload.chunks(188) {
			// adaptation-field-only packets (adaptation_field_control == 0b10) are the clock.
			if packet[3] & 0x30 != 0x20 {
				break;
			}
			assert_eq!(packet[5], 0x10, "PCR_flag alone, at {i}");
			// The six reserved bits between base and extension are ones (ISO 13818-1);
			// the crate's serializer writes zeros here, which is why the packet is
			// laid out by hand.
			assert_eq!(packet[10] & 0x7e, 0x7e, "reserved bits must be ones, at {i}");
			let base = (u64::from(packet[6]) << 25)
				| (u64::from(packet[7]) << 17)
				| (u64::from(packet[8]) << 9)
				| (u64::from(packet[9]) << 1)
				| u64::from(packet[10] >> 7);
			pcrs.push(base);
			head = Some(base);
		}
		// The frame is paced at the slot its last leading clock asserts (plus the
		// reserve), which is the newest slot that has begun by then.
		let Some(base) = head else { continue };
		let slot_ticks = frame.timestamp.as_micros() / 25_000 * 25_000 * 90 / 1_000;
		assert_eq!(
			base,
			(slot_ticks as u64).wrapping_sub(16) & WIRE,
			"pacing off value, at {i}"
		);
	}

	const WIRE: u64 = (1 << 33) - 1;
	assert!(pcrs.len() >= 2, "expected at least two grid slots, got {pcrs:?}");
	// Slot 0 minus the 16-tick default reserve, mod 2^33.
	assert_eq!(pcrs[0], WIRE - 15, "slot 0 backs off through the wrap: {pcrs:?}");
	// Every step is exactly one 25 ms slot (2250 ticks) in the circular clock.
	for (i, w) in pcrs.windows(2).enumerate() {
		assert_eq!(w[1].wrapping_sub(w[0]) & WIRE, 2250, "step off the grid at {i}: {w:?}");
	}
}

/// The clock backs off by the largest reserve of any track, not just the PCR
/// track's: a second rendition with a deeper reorder (catalog `jitter`) authors
/// its DTS further behind the PTS, and a clock respecting only the PCR track's
/// reserve would run ahead of those frames' decode times.
#[tokio::test(start_paused = true)]
async fn export_pcr_respects_every_renditions_reserve() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap();
	let mut make = |name: &str, jitter: Option<Duration>| {
		let track = broadcast.create_track(name, hang::container::track_info()).unwrap();
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: false,
		});
		cfg.container = Container::Legacy;
		cfg.description = Some(avcc.clone());
		cfg.jitter = jitter;
		catalog.lock().video.renditions.insert(name.to_string(), cfg);
		Producer::new(track, HangContainer::Legacy)
	};
	// "a" gets the lowest PID and so carries the PCR, with the tiny default
	// reserve; "b" declares a 100 ms reorder depth.
	let mut a = make("a.avc1", None);
	let mut b = make("b.avc1", Some(Duration::from_millis(100)));

	let idr = [0x65u8; 32];
	for i in 0..25u64 {
		let timestamp = Timestamp::from_millis(10_000 + i * 40).unwrap();
		for video in [&mut a, &mut b] {
			video
				.write(Frame {
					timestamp,
					duration: None,
					payload: length_prefixed(&[&idr]),
					keyframe: true,
				})
				.unwrap();
		}
	}
	a.finish().unwrap();
	b.finish().unwrap();

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	// In transport order, every PES unit (either rendition) decodes at or after
	// the last PCR preceding it.
	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut last_pcr = None;
	let mut units = 0usize;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(pcr) = packet.adaptation_field.as_ref().and_then(|af| af.pcr) {
			last_pcr = Some(pcr.as_u64() / 300);
		}
		if let Some(TsPayload::PesStart(pes)) = packet.payload {
			units += 1;
			let pts = pes.header.pts.expect("PES carried no PTS").as_u64();
			let decode = pes.header.dts.map(|t| t.as_u64()).unwrap_or(pts);
			if let Some(pcr) = last_pcr {
				assert!(decode >= pcr, "unit decodes at {decode}, before the clock at {pcr}");
			}
		}
	}
	assert!(units >= 50, "expected both renditions' units, got {units}");
}

/// A frame cadence coarser than the grid backfills every missed slot: low-rate
/// video-only content (here 2.5 fps, 16 slots per frame) still asserts a uniform
/// 25 ms ramp. A tight backfill cap would skip slots on every frame and re-create
/// the clock jumps this grid exists to eliminate.
#[tokio::test(start_paused = true)]
async fn export_pcr_backfills_a_coarse_cadence() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap();
	let track = broadcast
		.create_track(broadcast.unique_name(".avc1"), hang::container::track_info())
		.unwrap();
	{
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: false,
		});
		cfg.container = Container::Legacy;
		cfg.description = Some(avcc);
		catalog.lock().video.renditions.insert(track.name().to_string(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	let idr = [0x65u8; 32];
	for i in 0..10u64 {
		producer
			.write(Frame {
				timestamp: Timestamp::from_millis(10_000 + i * 400).unwrap(),
				duration: None,
				payload: length_prefixed(&[&idr]),
				keyframe: true,
			})
			.unwrap();
	}
	producer.finish().unwrap();

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut pcrs: Vec<u64> = Vec::new();
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(pcr) = packet.adaptation_field.as_ref().and_then(|af| af.pcr) {
			pcrs.push(pcr.as_u64() / 300);
		}
	}

	// 9 inter-frame spans of 400 ms, 16 slots each: every one backfilled.
	assert!(
		pcrs.len() > 100,
		"expected a dense backfilled clock, got {}",
		pcrs.len()
	);
	for (i, w) in pcrs.windows(2).enumerate() {
		assert_eq!(w[1] - w[0], 2250, "PCR interval off the grid at {i}: {w:?}");
	}
}

/// Full SCTE-35 round-trip: import `bbb.ts` (real H.264 + AAC) into a broadcast
/// that also carries a `.scte35` cue track, export to TS, re-import, and assert
/// the splice_info_section came back byte-for-byte. The PMT must advertise the
/// SCTE-35 stream (0x86) and the program-level CUEI registration descriptor.
#[tokio::test(start_paused = true)]
async fn export_scte35_roundtrip() {
	let data = include_bytes!("test_data/bbb.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// Create and write the SCTE-35 cue track BEFORE moving `broadcast` into
	// `Import` (which consumes it); the producer stays alive so the exporter can
	// subscribe to the retained track.
	let scte = broadcast
		.unique_track(".scte35", hang::container::track_info())
		.unwrap();
	let scte_name = scte.name().to_string();
	{
		let track = tscat::Track {
			pid: 0x102,
			descriptors: Vec::new(),
			verbatim: Some(tscat::Verbatim::new(0x86, tscat::Framing::Section)),
		};
		catalog.lock().mpegts.tracks.insert(scte_name.clone(), track);
	}
	let mut scte_producer = Producer::new(scte, HangContainer::Legacy);
	// bbb's first video keyframe is at 1.4 s; stamp the cue just after it so it survives
	// the tune-in alignment (a cue before the first keyframe is dropped with the lead).
	scte_producer
		.write(Frame {
			timestamp: Timestamp::from_millis(1410).unwrap(),
			duration: None,
			payload: Bytes::from_static(CUE),
			keyframe: true,
		})
		.unwrap();
	scte_producer.cut(None).unwrap();
	scte_producer.finish().unwrap();

	// Now add the real video/audio by importing bbb.ts (this moves `broadcast`).
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	// `import`, `catalog`, and `scte_producer` stay alive: retained tracks. The
	// exporter must carry the extension to see the mpegts section.
	let ts = drain_with(
		Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
			.await
			.unwrap(),
	)
	.await;
	assert_packet_aligned(&ts);

	// The first PMT advertises the SCTE-35 ES (0x86) and the CUEI descriptor.
	// Stop at it: the raw reader would choke on the SCTE section packets that
	// follow (the very reason the importer intercepts them).
	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut saw_scte_es = false;
	let mut saw_cuei = false;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::Pmt(pmt)) = packet.payload {
			saw_scte_es = pmt
				.es_info
				.iter()
				.any(|e| e.stream_type == StreamType::Dts8ChannelLosslessAudio);
			saw_cuei = pmt
				.program_info
				.iter()
				.any(|d| d.tag == 0x05 && d.data.len() >= 4 && &d.data[0..4] == b"CUEI");
			break;
		}
	}
	assert!(saw_scte_es, "PMT missing the SCTE-35 elementary stream (0x86)");
	assert!(saw_cuei, "PMT missing the program-level CUEI registration descriptor");

	// Re-import the exported TS and read the .scte35 frame back.
	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 =
		crate::catalog::Producer::with_catalog(&mut broadcast2, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
	import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
	import2.finish().unwrap();

	let snapshot = catalog2.snapshot();
	let verbatim = snapshot.mpegts.tracks.values().filter(|t| t.verbatim.is_some()).count();
	assert_eq!(verbatim, 1, "round-trip lost the SCTE-35 track");
	let name = scte_track(&snapshot).expect("a scte35 track");

	let track = consumer2.track(&name).unwrap().subscribe(None).await.unwrap();
	let mut scte_reader = crate::container::Consumer::new(track, HangContainer::Legacy);
	let frame = scte_reader
		.read()
		.await
		.unwrap()
		.expect("no SCTE-35 frame after round-trip");
	assert_eq!(
		frame.payload.as_ref(),
		CUE,
		"SCTE-35 section did not round-trip byte-for-byte"
	);
}

/// PES-framed verbatim round-trip: import `bbb.ts` (real H.264 + AAC, whose video
/// supplies the media clock the exporter needs) alongside a private PES-framed
/// stream (stream_type 0x06) carried verbatim, export to TS, then re-import and
/// assert the PID, framing, stream_id, and payload all survive. Exercises the
/// exporter's PES re-emit path; `private_pes_carried_verbatim` only covers import.
#[tokio::test(start_paused = true)]
async fn export_pes_verbatim_roundtrip() {
	const DATA_PID: u16 = 0x104;
	const STREAM_ID: u8 = 0xc0;
	const PAYLOAD: &[u8] = &[0xde, 0xad, 0xbe, 0xef, 0x01, 0x02];

	let data = include_bytes!("test_data/bbb.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// Build the verbatim PES track BEFORE moving `broadcast` into `Import`; the
	// producer stays alive so the exporter can subscribe to the retained track.
	let data_track = broadcast.unique_track(".data", hang::container::track_info()).unwrap();
	let data_name = data_track.name().to_string();
	{
		let mut verbatim = tscat::Verbatim::new(0x06, tscat::Framing::Pes);
		verbatim.stream_id = Some(STREAM_ID);
		let mut track = tscat::Track::new(DATA_PID);
		track.verbatim = Some(verbatim);
		catalog.lock().mpegts.tracks.insert(data_name.clone(), track);
	}
	let mut data_producer = Producer::new(data_track, HangContainer::Legacy);
	// bbb's first video keyframe is at 1.4 s; stamp the PES just after it so it survives
	// the tune-in alignment (content before the first keyframe is dropped with the lead).
	data_producer
		.write(Frame {
			timestamp: Timestamp::from_millis(1410).unwrap(),
			duration: None,
			payload: Bytes::from_static(PAYLOAD),
			keyframe: true,
		})
		.unwrap();
	data_producer.cut(None).unwrap();
	data_producer.finish().unwrap();

	// Real video/audio supplies the media clock (moves `broadcast`).
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	// `import`, `catalog`, and `data_producer` stay alive: retained tracks.
	let ts = drain_with(
		Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
			.await
			.unwrap(),
	)
	.await;
	assert_packet_aligned(&ts);

	// Re-import the exported TS and recover the verbatim PES stream.
	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 =
		crate::catalog::Producer::with_catalog(&mut broadcast2, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
	import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
	import2.finish().unwrap();

	let snapshot = catalog2.snapshot();
	let (name, track) = snapshot
		.mpegts
		.tracks
		.iter()
		.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x06))
		.expect("verbatim PES survived the round-trip");
	assert_eq!(track.pid, DATA_PID, "PES PID preserved");
	let verbatim = track.verbatim.as_ref().unwrap();
	assert_eq!(verbatim.framing, tscat::Framing::Pes, "PES framing preserved");
	assert_eq!(verbatim.stream_id, Some(STREAM_ID), "PES stream_id preserved");
	let name = name.clone();

	let track = consumer2.track(&name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(track, HangContainer::Legacy);
	let frame = reader
		.read()
		.await
		.unwrap()
		.expect("no verbatim PES frame after round-trip");
	assert_eq!(
		frame.payload.as_ref(),
		PAYLOAD,
		"verbatim PES payload round-trips byte-for-byte"
	);
}

// SCTE-35 cues are clocked on video, so the exporter rejects a cue program with no video
// track rather than emitting cues pinned to zero.
#[tokio::test(start_paused = true)]
async fn scte35_without_video_export_is_rejected() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// A SCTE-35 cue track and nothing else.
	let scte = broadcast
		.unique_track(".scte35", hang::container::track_info())
		.unwrap();
	let scte_name = scte.name().to_string();
	{
		let track = tscat::Track {
			pid: 0x102,
			descriptors: Vec::new(),
			verbatim: Some(tscat::Verbatim::new(0x86, tscat::Framing::Section)),
		};
		catalog.lock().mpegts.tracks.insert(scte_name, track);
	}
	let mut producer = Producer::new(scte, HangContainer::Legacy);
	producer
		.write(Frame {
			timestamp: Timestamp::from_millis(0).unwrap(),
			duration: None,
			payload: Bytes::from_static(CUE),
			keyframe: true,
		})
		.unwrap();
	producer.cut(None).unwrap();
	producer.finish().unwrap();

	let mut exporter = Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let err = loop {
		match tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await {
			Ok(Ok(Some(_))) => continue,
			Ok(Ok(None)) => panic!("export completed; a cue program without video must be rejected"),
			Ok(Err(e)) => break e,
			Err(_) => panic!("export neither errored nor completed"),
		}
	};
	assert!(
		err.to_string().contains("requires a video track"),
		"expected a video-required rejection, got: {err}"
	);
}

/// Subscribe to a track and read every retained frame payload it holds.
async fn read_frames(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<Vec<u8>> {
	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(track, HangContainer::Legacy);
	let mut frames = Vec::new();
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read()).await {
		let Some(frame) = res.unwrap() else { break };
		frames.push(frame.payload.to_vec());
	}
	frames
}

/// Both real Kyrion MP2 programs must survive TS -> MoQ -> TS byte-for-byte, and
/// the PMT must re-announce them as MPEG-1 audio (0x03): the capture is 48 kHz,
/// so the half-rate type (0x04) would be unfaithful. This capture is a dirty start
/// (begins mid-GOP), so the export's keyframe alignment drops the MP2 ahead of the
/// first video keyframe; what remains is a byte-exact suffix of each program.
#[tokio::test(start_paused = true)]
async fn mp2_kyrion_roundtrip_byte_exact() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	let names: Vec<String> = catalog.snapshot().audio.renditions.keys().cloned().collect();
	assert_eq!(names.len(), 2, "both Kyrion MP2 programs");
	let mut ingested = Vec::new();
	for name in &names {
		let frames = read_frames(&consumer, name).await;
		assert!(!frames.is_empty(), "{name}: no MP2 frames");
		assert!(
			frames.iter().all(|f| f[0] == 0xFF && f[1] & 0xE0 == 0xE0),
			"{name}: whole-frame carriage starts at the Layer II sync word"
		);
		ingested.push(frames);
	}

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::Pmt(pmt)) = packet.payload {
			let mp2 = pmt
				.es_info
				.iter()
				.filter(|e| e.stream_type == StreamType::Mpeg1Audio)
				.count();
			assert_eq!(mp2, 2, "PMT must re-announce both MP2 streams as 0x03");
			break;
		}
	}

	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
	import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
	import2.finish().unwrap();

	let names2: Vec<String> = catalog2.snapshot().audio.renditions.keys().cloned().collect();
	assert_eq!(names2.len(), 2, "round-trip lost an MP2 track");
	let mut roundtripped = Vec::new();
	for name in &names2 {
		roundtripped.push(read_frames(&consumer2, name).await);
	}

	// Keyframe alignment drops the MP2 ahead of the first video keyframe (the dirty-start
	// lead), so each program's surviving frames are a byte-exact suffix of what was
	// ingested. Track discovery order is not stable across imports, so match by content.
	for rt in &roundtripped {
		assert!(!rt.is_empty(), "a program lost all of its MP2 frames");
		assert!(
			ingested.iter().any(|ing| ing.ends_with(rt)),
			"round-tripped MP2 must be a byte-exact suffix of an ingested program"
		);
	}
}

/// The ffmpeg AC-3 fixture must survive TS -> MoQ -> TS byte-for-byte in an
/// audio-only program: the PCR falls to the audio track and the PMT re-announces
/// ATSC 0x81 with the 'AC-3' registration descriptor.
#[tokio::test(start_paused = true)]
async fn ac3_roundtrip_byte_exact() {
	let data = include_bytes!("test_data/ac3.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	let name = catalog
		.snapshot()
		.audio
		.renditions
		.keys()
		.next()
		.expect("an AC-3 track")
		.clone();
	let ingested = read_frames(&consumer, &name).await;
	assert!(!ingested.is_empty(), "no AC-3 frames");
	assert!(
		ingested.iter().all(|f| f[0] == 0x0B && f[1] == 0x77),
		"whole-frame carriage starts at the AC-3 sync word"
	);

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut checked_pmt = false;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::Pmt(pmt)) = packet.payload {
			assert_eq!(pmt.es_info.len(), 1);
			assert_eq!(pmt.es_info[0].stream_type, StreamType::DolbyDigitalUpToSixChannelAudio);
			assert!(
				pmt.es_info[0]
					.descriptors
					.iter()
					.any(|d| d.tag == 0x05 && d.data.as_slice() == b"AC-3"),
				"PMT missing the ES-level 'AC-3' registration descriptor"
			);
			checked_pmt = true;
			break;
		}
	}
	assert!(checked_pmt, "missing PMT");

	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
	import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
	import2.finish().unwrap();

	let name2 = catalog2
		.snapshot()
		.audio
		.renditions
		.keys()
		.next()
		.expect("round-trip lost the AC-3 track")
		.clone();
	let roundtripped = read_frames(&consumer2, &name2).await;
	assert_eq!(roundtripped, ingested, "AC-3 frames must survive byte-for-byte");
}

/// The ffmpeg E-AC-3 fixture must survive TS -> MoQ -> TS byte-for-byte in an
/// audio-only program; the PMT re-announces ATSC 0x87 with the 'EAC3'
/// registration descriptor.
#[tokio::test(start_paused = true)]
async fn eac3_roundtrip_byte_exact() {
	let data = include_bytes!("test_data/eac3.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	let name = catalog
		.snapshot()
		.audio
		.renditions
		.keys()
		.next()
		.expect("an E-AC-3 track")
		.clone();
	let ingested = read_frames(&consumer, &name).await;
	assert!(!ingested.is_empty(), "no E-AC-3 frames");
	assert!(
		ingested.iter().all(|f| f[0] == 0x0B && f[1] == 0x77),
		"whole-frame carriage starts at the E-AC-3 sync word"
	);

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut checked_pmt = false;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::Pmt(pmt)) = packet.payload {
			assert_eq!(pmt.es_info.len(), 1);
			assert_eq!(
				pmt.es_info[0].stream_type,
				StreamType::DolbyDigitalPlusUpTo16ChannelAudioForAtsc
			);
			assert!(
				pmt.es_info[0]
					.descriptors
					.iter()
					.any(|d| d.tag == 0x05 && d.data.as_slice() == b"EAC3"),
				"PMT missing the ES-level 'EAC3' registration descriptor"
			);
			checked_pmt = true;
			break;
		}
	}
	assert!(checked_pmt, "missing PMT");

	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
	import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
	import2.finish().unwrap();

	let name2 = catalog2
		.snapshot()
		.audio
		.renditions
		.keys()
		.next()
		.expect("round-trip lost the E-AC-3 track")
		.clone();
	let roundtripped = read_frames(&consumer2, &name2).await;
	assert_eq!(roundtripped, ingested, "E-AC-3 frames must survive byte-for-byte");
}

/// Read every audio rendition's retained frames, keyed by codec string.
async fn read_audio_by_codec(
	consumer: &moq_net::broadcast::Consumer,
	catalog: &crate::catalog::Producer,
) -> std::collections::BTreeMap<String, Vec<Vec<u8>>> {
	let mut out = std::collections::BTreeMap::new();
	for (name, config) in &catalog.snapshot().audio.renditions {
		out.insert(config.codec.to_string(), read_frames(consumer, name).await);
	}
	out
}

/// The ATSC-compliance Kyrion capture (MPEG-2 video + AC-3 + MP2) must round-trip
/// both real audio streams byte-for-byte. The video is clock-only, so the
/// re-exported program is audio-only with the PCR on an audio PID, and the PMT
/// re-announces 0x81 (with the 'AC-3' registration descriptor, which the Kyrion
/// itself also emits) and 0x03.
#[tokio::test(start_paused = true)]
async fn kyrion_ac3_mp2_roundtrip_byte_exact() {
	let data = include_bytes!("test_data/kyrion_mpeg2av_ac3.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	let ingested = read_audio_by_codec(&consumer, &catalog).await;
	assert_eq!(
		ingested.keys().cloned().collect::<Vec<_>>(),
		["ac-3", "mp2"],
		"both real audio codecs cataloged"
	);
	assert!(
		ingested["ac-3"].iter().all(|f| f[0] == 0x0B && f[1] == 0x77),
		"AC-3 whole-frame carriage"
	);
	assert!(
		ingested["mp2"].iter().all(|f| f[0] == 0xFF && f[1] & 0xE0 == 0xE0),
		"MP2 whole-frame carriage"
	);

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::Pmt(pmt)) = packet.payload {
			assert_eq!(pmt.es_info.len(), 2, "audio-only program: AC-3 + MP2");
			let ac3 = pmt
				.es_info
				.iter()
				.find(|e| e.stream_type == StreamType::DolbyDigitalUpToSixChannelAudio)
				.expect("AC-3 ES re-announced as 0x81");
			assert!(
				ac3.descriptors
					.iter()
					.any(|d| d.tag == 0x05 && d.data.as_slice() == b"AC-3"),
				"AC-3 registration descriptor"
			);
			assert!(
				pmt.es_info.iter().any(|e| e.stream_type == StreamType::Mpeg1Audio),
				"MP2 re-announced as 0x03 (48 kHz is an MPEG-1 rate)"
			);
			break;
		}
	}

	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
	import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
	import2.finish().unwrap();

	let roundtripped = read_audio_by_codec(&consumer2, &catalog2).await;
	assert_eq!(roundtripped, ingested, "both audio streams survive byte-for-byte");
}

/// Find the SCTE-35 verbatim stream (stream_type 0x86) in a catalog snapshot. A
/// clip may carry other undecoded streams verbatim, so select by type, not order.
fn scte_track(snap: &crate::catalog::hang::Catalog<tscat::Ext>) -> Option<String> {
	snap.mpegts
		.tracks
		.iter()
		.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x86))
		.map(|(name, _)| name.clone())
}

/// Subscribe to a cue track and read every retained `splice_info_section` it holds.
async fn read_cues(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<(Vec<u8>, Timestamp)> {
	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(track, HangContainer::Legacy);
	let mut cues = Vec::new();
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read()).await {
		let Some(frame) = res.unwrap() else { break };
		cues.push((frame.payload.to_vec(), frame.timestamp));
	}
	cues
}

/// Full TS -> MoQ -> TS over fixtures carrying SCTE-35 cues; each section must survive the seam
/// byte-for-byte. Most are real-video clips with injected cues (regenerate via the `scte35_inject`
/// example); tsduck.ts is TSDuck-authored and kyrion_dirtystart.ts is a real-encoder capture. Add
/// a source by dropping a `.ts` in `test_data/scte35/` and listing it here.
///
/// The cues are independently valid SCTE-35: TSDuck (the authoritative toolkit) decodes every
/// section in every fixture with CRC32 OK. That decode is checked in next to each clip as
/// `<fixture>_tsduck.txt`; regenerate it via the `moq-tsduck` image (cue PID 0x21 for the injected
/// fixtures, 0x14d for the Kyrion capture):
/// `tsp -I file test_data/scte35/<fixture>.ts -P tables --pid <pid> -O drop > <fixture>_tsduck.txt`.
#[tokio::test(start_paused = true)]
async fn scte35_fixtures_survive_roundtrip() {
	// The corpus proves byte-exact survival across sources that each cover an axis no other does;
	// cue counts vary per fixture (five for the injected clips, ten on the wire for tsduck, six for
	// the Kyrion capture). For every cue we assert survival, a known splice_command_type, and that
	// the per-fixture distinct count holds (so a clip that lost variety to duplicates fails). Only
	// tsduck, whose cues we author, pins the exact command-type set.
	// (source, total cues, distinct cues, expected command-type set or empty, fixture bytes.)
	type Fixture = (&'static str, usize, usize, &'static [u8], &'static [u8]);
	let fixtures: &[Fixture] = &[
		// ffmpeg mpegts muxer, H.264 320x240 progressive, no audio: the baseline.
		("ffmpeg", 5, 5, &[], include_bytes!("test_data/scte35/ffmpeg.ts")),
		// GStreamer mpegtsmux, H.264 720x480 interlaced (480i) + AAC: a second muxer, SD
		// interlaced framing, and an audio track.
		("gst480i", 5, 5, &[], include_bytes!("test_data/scte35/gst480i.ts")),
		// Real BigBuckBunny frames, H.265 320x240 + Opus: a second video codec, real content,
		// and the WebCodec-friendly Opus path.
		("bbb5s", 5, 5, &[], include_bytes!("test_data/scte35/bbb5s.ts")),
		// TSDuck-authored: splice_null, splice_insert, time_signal, and a private_command (custom),
		// each re-sent with an advancing CC so the importer emits 5 distinct x2 = 10. The only
		// fixture covering section repetition, distinct from the byte-identical same-CC transport
		// duplicate the reassembler drops.
		(
			"tsduck",
			10,
			5,
			&[0x00, 0x05, 0x06, 0xff],
			include_bytes!("test_data/scte35/tsduck.ts"),
		),
		// Real Ateme Kyrion broadcast (H.264 1080i + MP2), captured mid-stream: a real
		// encoder's cues surviving the full round-trip, not a synthetic clip. Cues are external,
		// so the command-type set stays unpinned.
		(
			"kyrion_dirtystart",
			6,
			6,
			&[],
			include_bytes!("test_data/scte35/kyrion_dirtystart.ts"),
		),
	];

	// SCTE-35 splice_command_type lives at byte 13 of the splice_info_section.
	const KNOWN_SPLICE_COMMANDS: [u8; 6] = [0x00, 0x04, 0x05, 0x06, 0x07, 0xff];

	for (source, total, distinct, command_types, data) in fixtures {
		// Ingest the fixture.
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(
			&mut broadcast,
			crate::catalog::hang::Catalog::<tscat::Ext>::default(),
		)
		.unwrap();
		let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
		import.decode(&BytesMut::from(&data[..])).unwrap();
		import.finish().unwrap();

		let snap = catalog.snapshot();
		assert!(!snap.video.renditions.is_empty(), "{source}: video track from the clip");
		// Select the SCTE-35 stream by stream_type (0x86); a clip may also carry other
		// undecoded streams verbatim (e.g. Opus as private PES in bbb5s).
		let name = scte_track(&snap).expect("a scte35 track");
		let ingested = read_cues(&consumer, &name).await;
		assert_eq!(ingested.len(), *total, "{source}: {total} cues on ingest");
		assert!(
			ingested.iter().all(|(b, _)| b.first() == Some(&0xfc)),
			"{source}: every cue is a splice_info_section (table_id 0xFC)"
		);
		let unique: std::collections::HashSet<&Vec<u8>> = ingested.iter().map(|(b, _)| b).collect();
		assert_eq!(
			unique.len(),
			*distinct,
			"{source}: {distinct} distinct cue sections, not dups"
		);
		// Structural validity: every cue's splice_command_type is a known SCTE-35 command.
		assert!(
			ingested
				.iter()
				.all(|(b, _)| b.get(13).is_some_and(|t| KNOWN_SPLICE_COMMANDS.contains(t))),
			"{source}: every cue carries a known splice_command_type"
		);
		// For fixtures we author (tsduck), pin the exact set of command types present.
		if !command_types.is_empty() {
			let mut got: Vec<u8> = ingested.iter().filter_map(|(b, _)| b.get(13).copied()).collect();
			got.sort_unstable();
			got.dedup();
			assert_eq!(got.as_slice(), *command_types, "{source}: splice_command_type set");
		}
		assert!(
			ingested.iter().all(|(_, ts)| *ts != Timestamp::ZERO),
			"{source}: cues stamped with the video PTS, not zero"
		);

		// Export and re-ingest.
		let ts = drain_with(
			Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
				.await
				.unwrap(),
		)
		.await;
		assert_packet_aligned(&ts);

		let mut broadcast2 = moq_net::broadcast::Info::new().produce();
		let consumer2 = broadcast2.consume();
		let catalog2 = crate::catalog::Producer::with_catalog(
			&mut broadcast2,
			crate::catalog::hang::Catalog::<tscat::Ext>::default(),
		)
		.unwrap();
		let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
		import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
		import2.finish().unwrap();
		let name2 = scte_track(&catalog2.snapshot()).expect("a scte35 track");
		let roundtripped = read_cues(&consumer2, &name2).await;

		let before: Vec<&Vec<u8>> = ingested.iter().map(|(b, _)| b).collect();
		let after: Vec<&Vec<u8>> = roundtripped.iter().map(|(b, _)| b).collect();
		assert_eq!(
			after, before,
			"{source}: every section survived TS -> MoQ -> TS byte-for-byte"
		);
	}
}

/// Build a PSI section: `table_id`, the 12-bit `section_length` (covering `body` plus a
/// 4-byte CRC), then `body` and a dummy CRC. The reassembler carries it verbatim and
/// never validates the CRC, so the bytes only need a self-consistent length.
fn make_section(table_id: u8, body: &[u8]) -> Vec<u8> {
	let section_length = body.len() + 4;
	let mut s = vec![
		table_id,
		0xb0 | ((section_length >> 8) as u8 & 0x0f),
		(section_length & 0xff) as u8,
	];
	s.extend_from_slice(body);
	s.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
	s
}

/// Wrap a complete section in one PUSI TS packet on `pid` (pointer_field 0), padded to 188.
fn si_packet(pid: u16, section: &[u8]) -> Vec<u8> {
	let mut p = vec![0x47, 0x40 | ((pid >> 8) as u8 & 0x1f), (pid & 0xff) as u8, 0x10, 0x00];
	p.extend_from_slice(section);
	assert!(p.len() <= 188, "section overflows one TS packet");
	p.resize(188, 0xff);
	p
}

/// Split a complete section across several 188-byte TS packets on `pid`: a PUSI packet
/// (pointer_field 0) carrying the head, then continuation packets (PUSI clear, continuity
/// counter advancing) for the rest, the last padded with 0xff stuffing. Unlike `si_packet`
/// this reaches the multi-packet reassembly path (PUSI + continuity across packets).
fn si_packets_multi(pid: u16, section: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	let mut cc = 0u8;
	// First packet: PUSI set, pointer_field 0, then as much of the section as fits.
	let mut first = vec![
		0x47,
		0x40 | ((pid >> 8) as u8 & 0x1f),
		(pid & 0xff) as u8,
		0x10 | cc,
		0x00,
	];
	let head = section.len().min(188 - first.len());
	first.extend_from_slice(&section[..head]);
	first.resize(188, 0xff);
	out.extend_from_slice(&first);

	// Continuation packets: PUSI clear, continuity counter incremented per payload packet.
	let mut pos = head;
	while pos < section.len() {
		cc = (cc + 1) & 0x0f;
		let mut p = vec![0x47, (pid >> 8) as u8 & 0x1f, (pid & 0xff) as u8, 0x10 | cc];
		let take = (section.len() - pos).min(188 - p.len());
		p.extend_from_slice(&section[pos..pos + take]);
		p.resize(188, 0xff);
		out.extend_from_slice(&p);
		pos += take;
	}
	out
}

/// Decode an SDT Actual section's first service: `(service_type, provider, name)` from the
/// service_descriptor (tag 0x48). Enough to prove the service identity survived, no more.
fn parse_sdt_service(sec: &[u8]) -> (u8, String, String) {
	// header(8) + first service loop entry: service_id(2), flags(1), running/free + desc_len(2).
	let desc_loop_len = (((sec[11 + 3] & 0x0f) as usize) << 8) | sec[11 + 4] as usize;
	let mut d = 11 + 5;
	let end = d + desc_loop_len;
	while d < end {
		let (tag, len) = (sec[d], sec[d + 1] as usize);
		let body = &sec[d + 2..d + 2 + len];
		if tag == 0x48 {
			let service_type = body[0];
			let prov_len = body[1] as usize;
			let provider = String::from_utf8_lossy(&body[2..2 + prov_len]).into_owned();
			let name_len = body[2 + prov_len] as usize;
			let name = String::from_utf8_lossy(&body[3 + prov_len..3 + prov_len + name_len]).into_owned();
			return (service_type, provider, name);
		}
		d += 2 + len;
	}
	panic!("SDT service_descriptor (0x48) not found");
}

/// The DVB service layer (SDT + NIT + transport/service identity) must survive
/// TS -> MoQ -> TS. `bbb.ts` carries a real ffmpeg SDT (service "Service01" / provider
/// "FFmpeg"); no fixture carries a NIT, so a synthetic one is injected on PID 0x0010.
/// After the round-trip the SDT and NIT are byte-identical and the identity is preserved.
#[tokio::test(start_paused = true)]
async fn service_layer_survives_roundtrip() {
	let data = include_bytes!("test_data/bbb.ts");
	let nit = make_section(0x40, &[0x12, 0x34, 0xff, 0x01]);

	// Prepend a synthetic NIT Actual packet (0x0010); prepend keeps bbb's alignment.
	// Twice, because real SI repeats every few seconds: the repetition must collapse
	// into the same single section rather than accumulating a duplicate.
	let mut input = si_packet(0x0010, &nit);
	input.extend_from_slice(&si_packet(0x0010, &nit));
	input.extend_from_slice(&data[..]);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&input[..])).unwrap();
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	let program = snapshot.mpegts.program.clone().expect("a program record");
	assert_eq!(program.transport_stream_id, 1, "TSID captured from the PAT");
	assert_eq!(program.program_number, 1, "program number captured from the PAT");
	assert_eq!(program.pmt_pid, 0x1000, "original PMT PID captured from the PAT");
	let si = snapshot.mpegts.si.clone();
	let entry = |pid: u16| si.get(&pid).expect("an SI entry for the PID");

	assert_eq!(entry(0x0011).sections.len(), 1, "bbb.ts carries one SDT section");
	let sdt = entry(0x0011).sections[0].clone();
	assert_eq!(sdt.first(), Some(&0x42), "SDT Actual (table_id 0x42)");
	assert_eq!(
		entry(0x0011).interval,
		Some(std::time::Duration::from_secs(2)),
		"the DVB SDT interval was filled in"
	);
	let (service_type, provider, name) = parse_sdt_service(&sdt);
	assert_eq!(
		(service_type, provider.as_str(), name.as_str()),
		(0x01, "FFmpeg", "Service01")
	);
	assert_eq!(
		entry(0x0010).sections,
		vec![Bytes::from(make_section(0x40, &[0x12, 0x34, 0xff, 0x01]))],
		"the repeated NIT deduped to one section"
	);

	// `import` and `catalog` stay alive: retained tracks the exporter subscribes to.
	let ts = drain_with(
		Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
			.await
			.unwrap(),
	)
	.await;
	assert_packet_aligned(&ts);

	// The rebuilt PAT preserves the transport/service identity and PMT PID.
	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut checked_pat = false;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::Pat(pat)) = packet.payload {
			assert_eq!(pat.transport_stream_id, 1, "TSID preserved in the rebuilt PAT");
			assert_eq!(pat.table.len(), 1);
			assert_eq!(pat.table[0].program_num, 1, "service number preserved");
			assert_eq!(pat.table[0].program_map_pid.as_u16(), 0x1000, "PMT PID preserved");
			checked_pat = true;
			break;
		}
	}
	assert!(checked_pat, "missing PAT");

	// Re-import: the SDT and NIT must come back byte-for-byte.
	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let _consumer2 = broadcast2.consume();
	let catalog2 =
		crate::catalog::Producer::with_catalog(&mut broadcast2, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.reserve());
	import2.decode(&BytesMut::from(ts.as_ref())).unwrap();
	import2.finish().unwrap();

	let snapshot2 = catalog2.snapshot();
	let program2 = snapshot2
		.mpegts
		.program
		.clone()
		.expect("a program record after round-trip");
	assert_eq!(
		program2.transport_stream_id, program.transport_stream_id,
		"TSID survived"
	);
	assert_eq!(
		program2.program_number, program.program_number,
		"program number survived"
	);
	assert_eq!(program2.pmt_pid, program.pmt_pid, "PMT PID survived");
	assert_eq!(snapshot2.mpegts.si, si, "every SI PID survived byte-for-byte");
}

/// Each SI PID must be re-emitted on its own interval, independently of the PSI cadence
/// and of video keyframes. The fixtures are a fraction of a second long, so nothing else
/// distinguishes a correct interval from "emitted once and never again": this builds a
/// 12-second synthetic timeline where the SDT (2s) and NIT (10s) land a different number
/// of times, and neither matches the 13 keyframes that drive the PSI.
#[tokio::test(start_paused = true)]
async fn si_pids_are_re_emitted_on_their_own_interval() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap();
	let track = broadcast
		.create_track(broadcast.unique_name(".avc1"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut guard = catalog.lock();
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		cfg.container = Container::Legacy;
		cfg.description = Some(avcc);
		guard.video.renditions.insert(name.clone(), cfg);

		// SDT every 2s, NIT every 10s: the DVB maxima import fills in.
		guard.mpegts.si.insert(
			0x0011,
			tscat::Si {
				sections: vec![Bytes::from(make_section(0x42, &[0xaa; 8]))],
				interval: Some(Duration::from_secs(2)),
				..Default::default()
			},
		);
		guard.mpegts.si.insert(
			0x0010,
			tscat::Si {
				sections: vec![Bytes::from(make_section(0x40, &[0xbb; 8]))],
				interval: Some(Duration::from_secs(10)),
				..Default::default()
			},
		);
	}

	// One keyframe per second across 12s, so the PSI fires on every one of the 13.
	let mut producer = Producer::new(track, HangContainer::Legacy);
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 300));
	for sec in 0..=12u64 {
		producer
			.write(Frame {
				timestamp: Timestamp::from_micros(sec * 1_000_000).unwrap(),
				duration: None,
				payload: length_prefixed(&[&idr]),
				keyframe: true,
			})
			.unwrap();
		producer.cut(None).unwrap();
	}
	producer.finish().unwrap();

	let ts = drain_with(
		Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
			.await
			.unwrap(),
	)
	.await;
	assert_packet_aligned(&ts);

	let count = |pid: u16| {
		ts.chunks_exact(188)
			.filter(|p| ((((p[1] & 0x1f) as u16) << 8) | p[2] as u16) == pid)
			.count()
	};

	// Control: the PSI rides every output frame here (each is a keyframe, and they are
	// further apart than PSI_INTERVAL either way). The SI PIDs must not follow it.
	assert_eq!(count(0x0000), 13, "PAT on every frame");
	// SDT at 0,2,4,6,8,10,12s.
	assert_eq!(count(0x0011), 7, "SDT re-emitted on its 2s interval");
	// NIT at 0 and 10s.
	assert_eq!(count(0x0010), 2, "NIT re-emitted on its 10s interval");
}

/// Count the TS packets on `pid` across every frame.
fn count_pid(frames: &[Frame], pid: u16) -> usize {
	frames
		.iter()
		.flat_map(|f| f.payload.chunks_exact(188))
		.filter(|p| ((((p[1] & 0x1f) as u16) << 8) | p[2] as u16) == pid)
		.count()
}

/// Count the TS packets whose adaptation field sets `discontinuity_indicator`.
fn count_discontinuity(frames: &[Frame]) -> usize {
	frames
		.iter()
		.flat_map(|f| f.payload.chunks_exact(188))
		.filter(|p| p[3] & 0x20 != 0 && p[4] > 0 && p[5] & 0x80 != 0)
		.count()
}

/// A `mpegts`-extension exporter over an announced broadcast.
async fn export_of(consumer: &moq_net::broadcast::Consumer) -> Export<tscat::Ext> {
	Export::with_ts(crate::source::announced(consumer), crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap()
}

/// Regression for #2833: every cadence in the exporter is anchored on a media timestamp
/// that only moves forward, so a publisher rewind froze each one for the length of the
/// rewound span. An audio-only program is the worst case: the PSI has no keyframe to
/// recover at, and the PCR-only packet is the only adaptation field it ever writes.
#[tokio::test(start_paused = true)]
async fn rewind_re_emits_tables_and_resumes_the_clock() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".aac"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut guard = catalog.lock();
		let mut cfg = AudioConfig::new(AAC { profile: 2 }, 48_000, 2);
		cfg.container = Container::Legacy;
		guard.audio.renditions.insert(name, cfg);

		// An SDT on its DVB 2s maximum: the cadence with no keyframe escape hatch.
		guard.mpegts.si.insert(
			0x0011,
			tscat::Si {
				sections: vec![Bytes::from(make_section(0x42, &[0xaa; 8]))],
				interval: Some(Duration::from_secs(2)),
				..Default::default()
			},
		);
	}

	// 100ms frames in one-second groups.
	let write = |producer: &mut Producer<HangContainer>, count: u64, offset: u64| {
		for i in 0..count {
			producer
				.write(Frame {
					timestamp: Timestamp::from_micros(offset + i * 100_000).unwrap(),
					duration: None,
					payload: Bytes::from_iter((0..180u16).map(|b| (b ^ i as u16) as u8)),
					keyframe: i % 10 == 0,
				})
				.unwrap();
			if i % 10 == 9 {
				producer.cut(None).unwrap();
			}
		}
	};

	let mut producer = Producer::new(track, HangContainer::Legacy);
	let mut export = export_of(&consumer).await;

	// A ten-minute offset exercises the long rewind from the controlled stimulus
	// campaign. Drain five seconds before restarting at zero.
	write(&mut producer, 50, 600_000_000);
	let before = drain_frames(&mut export).await;
	assert_eq!(export.discontinuity(), 0, "no rewind yet");
	assert_eq!(count_pid(&before, 0x0000), 10, "PAT on its 500ms cadence");
	assert_eq!(count_pid(&before, 0x0011), 3, "SDT at 0, 2 and 4s");

	// A forward marker must flush the last old frame instead of discarding it.
	producer.discontinuity().unwrap();
	write(&mut producer, 2, 605_000_000);
	let marked = drain_frames(&mut export).await;
	let tail: Vec<_> = marked.iter().flat_map(|f| f.payload.iter().copied()).collect();
	let (_, audio_pts) = collect_pes_pts(&tail);
	// The carried tail may precede the refreshed PMT, so inspect all PES starts.
	let mut reader = TsPacketReader::new(Cursor::new(tail));
	let mut preserved = false;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::PesStart(pes)) = packet.payload {
			preserved |= pes.header.pts.unwrap().as_u64() == 604_900_000 * 90 / 1000;
		}
	}
	assert!(preserved, "forward marker discarded the pre-boundary tail");
	assert!(!audio_pts.is_empty());
	let epoch = export.discontinuity();

	// The publisher rewinds and replays the first two seconds.
	write(&mut producer, 20, 0);
	let after = drain_frames(&mut export).await;
	assert_eq!(export.discontinuity(), epoch + 1, "the rewind was observed");

	// The clock leads the new timeline: its first frame is the PCR for slot 0, and the
	// grid ramps from there instead of waiting to recross the old timeline.
	assert_eq!(
		after[0].payload[3] & 0x30,
		0x20,
		"the new timeline leads with its clock"
	);
	let pcrs = collect_pcrs(&after);
	assert!(pcrs.len() > 60, "clock resumed promptly");
	assert_eq!(after[0].timestamp.as_micros(), 0);
	for pair in pcrs.windows(2) {
		assert_eq!(pair[1].1.wrapping_sub(pair[0].1) & ((1 << 33) - 1), 2250);
	}
	assert!(after.iter().all(|f| f.timestamp.as_micros() < 2_000_000));

	// And the tables come back on cadence rather than waiting ten minutes.
	assert_eq!(count_pid(&after, 0x0000), 4, "PAT at 0, 0.5, 1 and 1.5s");
	assert_eq!(count_pid(&after, 0x0011), 1, "SDT at 0s");

	// Exactly one packet marks the break, and it is the leading PCR.
	assert_eq!(count_discontinuity(&before), 0);
	assert_eq!(count_discontinuity(&after), 1, "the break is flagged exactly once");
	assert_eq!(count_discontinuity(&after[..1]), 1, "flagged on the leading PCR packet");
}

/// The counter-case, and why the fix keys on the discontinuity counter rather than on a
/// backwards slot: video is emitted in decode order, so a reordered (B-frame) PTS steps
/// backwards constantly. Re-emitting on every backwards step was measured at 25x the
/// intended PSI rate, and none of those steps is a rewind.
#[tokio::test(start_paused = true)]
async fn reordered_video_keeps_the_table_cadence() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap();
	let track = broadcast
		.create_track(broadcast.unique_name(".avc1"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut guard = catalog.lock();
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		cfg.container = Container::Legacy;
		cfg.description = Some(avcc);
		guard.video.renditions.insert(name, cfg);
		guard.mpegts.si.insert(
			0x0011,
			tscat::Si {
				sections: vec![Bytes::from(make_section(0x42, &[0xaa; 8]))],
				interval: Some(Duration::from_secs(2)),
				..Default::default()
			},
		);
	}

	// One group, one keyframe: the PSI has no keyframe boundary to hide behind, so what
	// is counted below is the interval cadence alone. 125 frames at 40ms displayed,
	// emitted in decode order as IPBB quads, so every fourth timestamp jumps 120ms ahead
	// and the next two step back behind it.
	let mut producer = Producer::new(track, HangContainer::Legacy);
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 300));
	let mut slice = vec![0x41u8];
	slice.extend(std::iter::repeat_n(0xCD, 200));
	for quad in 0..32u64 {
		for offset in [0, 3, 1, 2] {
			let index = quad * 4 + offset;
			if index >= 125 {
				continue;
			}
			let keyframe = index == 0;
			producer
				.write(Frame {
					timestamp: Timestamp::from_micros(index * 40_000).unwrap(),
					duration: None,
					payload: length_prefixed(&[if keyframe { idr.as_slice() } else { slice.as_slice() }]),
					keyframe,
				})
				.unwrap();
		}
	}

	let mut export = export_of(&consumer).await;
	let out = drain_frames(&mut export).await;

	// 125 frames span 0..4.96s: ten 500ms slots and three 2s slots, one emission each.
	assert_eq!(export.discontinuity(), 0, "a reorder is not a rewind");
	assert_eq!(count_pid(&out, 0x0000), 10, "PAT once per 500ms slot, not per reorder");
	assert_eq!(count_pid(&out, 0x0011), 3, "SDT once per 2s slot, not per reorder");
	assert_eq!(count_discontinuity(&out), 0, "a reorder is not a discontinuity");
}

/// A program with more than one track marks the break once, not once per track. Each
/// rendition carries its own discontinuity counter, and the rewound frame sorts ahead of
/// any old-epoch straggler still buffered on the other track. Each track joins the
/// new program epoch on its own boundary, without comparing unrelated counter values.
#[tokio::test(start_paused = true)]
async fn rewind_flags_the_break_once_across_tracks() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap();
	let video_track = broadcast
		.create_track(broadcast.unique_name(".avc1"), hang::container::track_info())
		.unwrap();
	let audio_track = broadcast
		.create_track(broadcast.unique_name(".aac"), hang::container::track_info())
		.unwrap();
	{
		let mut guard = catalog.lock();
		let mut video = VideoConfig::new(H264 {
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		video.container = Container::Legacy;
		video.description = Some(avcc);
		guard.video.renditions.insert(video_track.name().to_string(), video);

		let mut audio = AudioConfig::new(AAC { profile: 2 }, 48_000, 2);
		audio.container = Container::Legacy;
		guard.audio.renditions.insert(audio_track.name().to_string(), audio);
	}

	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 300));
	let mut video = Producer::new(video_track, HangContainer::Legacy);
	let mut audio = Producer::new(audio_track, HangContainer::Legacy);

	// One keyframe-led second per group on video, 100ms audio frames alongside it.
	let write = |video: &mut Producer<HangContainer>, audio: &mut Producer<HangContainer>, seconds: u64| {
		for sec in 0..seconds {
			video
				.write(Frame {
					timestamp: Timestamp::from_micros(sec * 1_000_000).unwrap(),
					duration: None,
					payload: length_prefixed(&[idr.as_slice()]),
					keyframe: true,
				})
				.unwrap();
			video.cut(None).unwrap();
			for tenth in 0..10u64 {
				audio
					.write(Frame {
						timestamp: Timestamp::from_micros(sec * 1_000_000 + tenth * 100_000).unwrap(),
						duration: None,
						payload: Bytes::from_iter((0..180u16).map(|b| (b ^ tenth as u16) as u8)),
						keyframe: tenth == 0,
					})
					.unwrap();
			}
			audio.cut(None).unwrap();
		}
	};

	let mut export = export_of(&consumer).await;
	write(&mut video, &mut audio, 4);
	let before = drain_frames(&mut export).await;
	assert_eq!(export.discontinuity(), 0);
	assert_eq!(count_discontinuity(&before), 0);

	// Independent counters can differ before the shared rewind. Forward markers
	// affect only video; audio must not be fenced out of a continuous timeline.
	for _ in 0..3 {
		video.discontinuity().unwrap();
	}
	video
		.write(Frame {
			timestamp: Timestamp::from_micros(4_000_000).unwrap(),
			duration: None,
			payload: length_prefixed(&[idr.as_slice()]),
			keyframe: true,
		})
		.unwrap();
	video.cut(None).unwrap();
	video
		.write(Frame {
			timestamp: Timestamp::from_micros(4_100_000).unwrap(),
			duration: None,
			payload: length_prefixed(&[idr.as_slice()]),
			keyframe: true,
		})
		.unwrap();
	video.cut(None).unwrap();
	let marked = drain_frames(&mut export).await;
	assert_eq!(export.discontinuity(), 1, "local marker counts are not program epochs");
	let epoch = export.discontinuity();

	// Both tracks rewind to zero.
	write(&mut video, &mut audio, 2);
	let after = drain_frames(&mut export).await;

	assert_eq!(
		export.discontinuity(),
		epoch + 1,
		"one rewind, however many tracks saw it"
	);
	assert_eq!(
		after[0].payload[3] & 0x30,
		0x20,
		"the new timeline leads with its clock"
	);
	assert_eq!(after[0].timestamp, Timestamp::from_micros(0).unwrap());
	assert_eq!(count_discontinuity(&after), 1, "the break is flagged exactly once");
	// Both renditions carry the rewound span; audio is not held back by a tune-in
	// anchor that belongs to the old timeline.
	assert!(count_pid(&after, 0x1001) > 0, "video resumed");
	assert!(count_pid(&after, 0x1002) > 0, "audio resumed");
	let bytes: Vec<_> = after.iter().flat_map(|f| f.payload.iter().copied()).collect();
	let mut reader = TsPacketReader::new(Cursor::new(bytes));
	let mut video_frames = 0;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if packet.header.pid.as_u16() == 0x1001
			&& let Some(TsPayload::PesStart(pes)) = packet.payload
		{
			let pts = pes.header.pts.unwrap().as_u64();
			assert!(
				pes.header.dts.is_none_or(|dts| dts.as_u64() <= pts),
				"DTS retained the old epoch"
			);
			video_frames += 1;
		}
	}
	assert!(video_frames > 0);

	// Keep old video queued while the lower-count audio source rewinds again.
	// It must start a new program epoch despite having the smaller counter.
	video.discontinuity().unwrap();
	video
		.write(Frame {
			timestamp: Timestamp::from_micros(8_000_000).unwrap(),
			duration: None,
			payload: length_prefixed(&[idr.as_slice()]),
			keyframe: true,
		})
		.unwrap();
	video.cut(None).unwrap();
	for i in 0..20 {
		audio
			.write(Frame {
				timestamp: Timestamp::from_micros(i * 100_000).unwrap(),
				duration: None,
				payload: Bytes::from_static(&[0xaa; 180]),
				keyframe: i == 0,
			})
			.unwrap();
	}
	audio.cut(None).unwrap();
	let again = drain_frames(&mut export).await;
	assert_eq!(export.discontinuity(), epoch + 2);
	assert_eq!(count_discontinuity(&again), 1);
	assert!(again.len() > 20, "rewound audio kept emitting without its stale peer");
	assert!(
		again.iter().all(|f| f.timestamp.as_micros() < 2_000_000),
		"stale video advanced the clock"
	);
	assert_eq!(count_pid(&again, 0x1001), 0, "old video was discarded");

	// An unrelated old-timeline marker cannot admit the peer's stale clock.
	video.discontinuity().unwrap();
	video
		.write(Frame {
			timestamp: Timestamp::from_micros(9_000_000).unwrap(),
			duration: None,
			payload: length_prefixed(&[idr.as_slice()]),
			keyframe: true,
		})
		.unwrap();
	video.cut(None).unwrap();
	assert!(drain_frames(&mut export).await.is_empty());

	// A delayed peer joins on its own boundary without resetting the clock again.
	for i in 0..4 {
		video
			.write(Frame {
				timestamp: Timestamp::from_micros(i * 1_000_000).unwrap(),
				duration: None,
				payload: length_prefixed(&[idr.as_slice()]),
				keyframe: true,
			})
			.unwrap();
		video.cut(None).unwrap();
	}
	let joined = drain_frames(&mut export).await;
	assert_eq!(export.discontinuity(), epoch + 2);
	assert_eq!(count_discontinuity(&joined), 0);
	assert!(count_pid(&joined, 0x1001) > 0, "video rejoined");
	// A discarded span already had counters assigned. They must roll back to
	// the last emitted bytes, so dropping the span introduces no packet loss.
	let mut counters = std::collections::HashMap::new();
	for frame in before.iter().chain(&marked).chain(&after).chain(&again).chain(&joined) {
		for packet in frame.payload.chunks_exact(188) {
			let pid = u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2]);
			let cc = packet[3] & 15;
			if let Some(prev) = counters.insert(pid, cc) {
				let expected = (prev + u8::from(packet[3] & 0x10 != 0)) & 15;
				assert_eq!(cc, expected, "counter gap on PID {pid}");
			}
		}
	}
}

/// A DVB SI section larger than one TS packet must be reassembled and captured verbatim.
/// `si_packet` only covers a single-packet section; a real SDT with several services (or a
/// NIT) spans packets, exercising the `SectionReassembler` PUSI + continuity path that
/// feeds `service.sdt`. The body is arbitrary here (capture is verbatim, not parsed).
#[test]
fn multi_packet_si_section_is_captured() {
	// 400-byte body forces the SDT Actual across three TS packets (183 + 184 + rest).
	let body: Vec<u8> = (0..400u16).map(|i| i as u8).collect();
	let sdt = make_section(0x42, &body);
	let input = si_packets_multi(0x0011, &sdt);
	assert!(input.len() > 188, "the SDT must span more than one TS packet");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let _consumer = broadcast.consume();
	let catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&input[..])).unwrap();
	import.finish().unwrap();

	let si = catalog.snapshot().mpegts.si.clone();
	assert_eq!(
		si.get(&0x0011).expect("an SDT entry").sections,
		vec![Bytes::from(sdt)],
		"the multi-packet SDT was reassembled and captured byte-for-byte"
	);
}

/// A raw Opus packet: a one-byte TOC (config 1 = SILK NB 20 ms, stereo, code 0) plus
/// `len` filler bytes, so it parses as one 20 ms frame.
fn opus_packet(fill: u8, len: usize) -> Bytes {
	let mut p = vec![(1 << 3) | (1 << 2)]; // TOC: config=1, s=1 (stereo), c=0
	p.extend(std::iter::repeat_n(fill, len));
	Bytes::from(p)
}

/// Strip the Opus-in-TS control header from a PES payload, returning the raw packets it
/// carries. Assumes no trim / no control extension (what the exporter emits).
fn strip_opus_control(mut data: &[u8]) -> Vec<Vec<u8>> {
	let mut packets = Vec::new();
	while !data.is_empty() {
		assert_eq!(data[0], 0x7f, "control header sync byte 0");
		assert_eq!(data[1] & 0xe0, 0xe0, "control header sync byte 1");
		assert_eq!(data[1] & 0x1c, 0x00, "exporter emits no trim / extension flags");
		let mut pos = 2;
		let mut size = 0usize;
		loop {
			let b = data[pos];
			pos += 1;
			size += b as usize;
			if b != 0xff {
				break;
			}
		}
		packets.push(data[pos..pos + size].to_vec());
		data = &data[pos + size..];
	}
	packets
}

/// Export an Opus broadcast and assert the program tables advertise a private-data
/// (0x06) stream carrying the 'Opus' registration + DVB extension descriptors, and that
/// the control-header-wrapped PES recovers the raw Opus packets with the right PTS.
#[tokio::test(start_paused = true)]
async fn export_opus_roundtrip() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".opus"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut cfg = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		cfg.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(name.clone(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	// The last packet is > 184 bytes to force PES splitting across TS packets.
	let packets: Vec<Bytes> = vec![opus_packet(0x01, 4), opus_packet(0x10, 8), opus_packet(0x20, 200)];
	for (i, payload) in packets.iter().enumerate() {
		producer
			.write(Frame {
				timestamp: Timestamp::from_micros(i as u64 * 20_000).unwrap(),
				duration: None,
				payload: payload.clone(),
				keyframe: true,
			})
			.unwrap();
		producer.cut(None).unwrap();
	}
	producer.finish().unwrap();

	let ts = drain(consumer).await;
	assert_packet_aligned(&ts);

	// Pass 1: one private-data stream with the Opus registration + extension descriptors.
	let mut reader = TsPacketReader::new(Cursor::new(ts.as_ref()));
	let mut saw_pmt = false;
	while let Some(packet) = reader.read_ts_packet().unwrap() {
		if let Some(TsPayload::Pmt(pmt)) = packet.payload {
			saw_pmt = true;
			assert_eq!(pmt.es_info.len(), 1);
			let es = &pmt.es_info[0];
			assert_eq!(es.stream_type, StreamType::from_u8(0x06).unwrap());
			let reg = es.descriptors.iter().find(|d| d.tag == 0x05).expect("registration");
			assert_eq!(reg.data, b"Opus");
			let ext = es.descriptors.iter().find(|d| d.tag == 0x7f).expect("extension");
			assert_eq!(ext.data, vec![0x80, 0x02], "ext tag 0x80 + stereo channel_config_code");
		}
	}
	assert!(saw_pmt, "missing PMT");

	// Pass 2: reassemble PES packets and recover the raw Opus packets.
	let mut pes = PesPacketReader::new(TsPacketReader::new(Cursor::new(ts.as_ref())));
	let mut recovered: Vec<(u64, Vec<u8>)> = Vec::new();
	while let Some(packet) = pes.read_pes_packet().unwrap() {
		assert_eq!(
			packet.header.stream_id.as_u8(),
			mpeg2ts::es::StreamId::PRIVATE_STREAM_1,
			"Opus rides private_stream_1"
		);
		let pts = packet.header.pts.expect("PES carried no PTS").as_u64();
		for raw in strip_opus_control(&packet.data) {
			recovered.push((pts, raw));
		}
	}

	assert_eq!(recovered.len(), packets.len());
	for (i, payload) in packets.iter().enumerate() {
		let (pts, raw) = &recovered[i];
		assert_eq!(*pts, i as u64 * 20 * 90, "PTS should be ms * 90 (90 kHz)");
		assert_eq!(raw.as_slice(), payload.as_ref(), "raw Opus payload mismatch");
	}
}

/// Round-trip an Opus broadcast through TS and back: export, re-import, and confirm the
/// catalog surfaces one 48 kHz Opus track whose frames recover the original packets.
#[tokio::test(start_paused = true)]
async fn opus_export_import_roundtrip() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".opus"), hang::container::track_info())
		.unwrap();
	let name = track.name().to_string();
	{
		let mut cfg = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		cfg.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(name.clone(), cfg);
	}
	let mut producer = Producer::new(track, HangContainer::Legacy);

	let packets: Vec<Bytes> = (0..4).map(|i| opus_packet(0x40 + i as u8, 24)).collect();
	for (i, payload) in packets.iter().enumerate() {
		producer
			.write(Frame {
				timestamp: Timestamp::from_micros(i as u64 * 20_000).unwrap(),
				duration: None,
				payload: payload.clone(),
				keyframe: true,
			})
			.unwrap();
		producer.cut(None).unwrap();
	}
	producer.finish().unwrap();

	let ts = drain(consumer).await;

	// Re-import the TS we just produced.
	let mut imported = moq_net::broadcast::Info::new().produce();
	let imported_consumer = imported.consume();
	let import_catalog = crate::catalog::Producer::new(&mut imported).unwrap();
	let mut import = crate::container::ts::Import::new(imported, import_catalog.reserve());
	import.decode(&ts).unwrap();
	import.finish().unwrap();

	let snapshot = import_catalog.snapshot();
	assert_eq!(snapshot.audio.renditions.len(), 1, "expected one Opus track");
	let (opus_name, audio) = snapshot.audio.renditions.iter().next().unwrap();
	assert_eq!(audio.codec.to_string(), "opus");
	assert_eq!(audio.sample_rate, 48_000);
	assert_eq!(audio.channel_count, 2);

	// The imported packets must match what we published.
	let recovered = read_frames(&imported_consumer, opus_name).await;
	assert_eq!(recovered.len(), packets.len(), "frame count");
	for (orig, got) in packets.iter().zip(&recovered) {
		assert_eq!(got.as_slice(), orig.as_ref(), "Opus packet survived the round-trip");
	}
}

// Two exporters of one broadcast, started at different times, must render the same packets
// from the moment they overlap. That is what a redundant (SMPTE ST 2022-7) pair compares, and
// it is what lets a leg be restarted without the merge at the far end seeing the two disagree.
// See moq-dev/moq#2779.

/// 25 fps video.
const VIDEO_US: u64 = 40_000;
/// 48 kHz AAC, 1024 samples per frame.
const AUDIO_US: u64 = 21_333;
/// Video frames per group. Deliberately not a whole number of PSI intervals, so a table
/// cadence anchored anywhere but the media timeline drifts against the keyframes.
const GOP: u64 = 15;
/// Audio frames per group, roughly matching the video group duration.
const AUDIO_GROUP: u64 = 28;
/// Video-frame ticks to produce, and the tick the second exporter joins at.
const TICKS: u64 = 150;
const JOIN: u64 = 75;

/// Produce one broadcast and export it twice, the second exporter joining partway in, and
/// return what each rendered.
///
/// Both exporters are drained after every write, so neither skips a group and the two see the
/// same arrival order. That isolates the question under test (does the *rendering* depend on
/// when the process started) from the separate question of whether two legs received the same
/// groups in the same order.
async fn export_twice(with_video: bool) -> (Vec<Frame>, Vec<Frame>) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let mut video = with_video.then(|| {
		let track = broadcast
			.create_track(broadcast.unique_name(".avc1"), hang::container::track_info())
			.unwrap();
		// Out-of-band parameter sets (avc1), so the export source takes the catalog
		// description as-is instead of parsing them out of the bitstream.
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: false,
		});
		cfg.container = Container::Legacy;
		cfg.description =
			Some(crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap());
		catalog.lock().video.renditions.insert(track.name().to_string(), cfg);
		Producer::new(track, HangContainer::Legacy)
	});

	let mut audio = {
		let track = broadcast
			.create_track(broadcast.unique_name(".aac"), hang::container::track_info())
			.unwrap();
		let mut cfg = AudioConfig::new(AAC { profile: 2 }, 48_000, 2);
		cfg.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(track.name().to_string(), cfg);
		Producer::new(track, HangContainer::Legacy)
	};

	let source = crate::source::announced(&consumer);
	let mut a = Export::new(source.clone()).await.unwrap();
	let mut b = None;
	let (mut out_a, mut out_b) = (Vec::new(), Vec::new());

	let mut audio_index = 0;
	for tick in 0..TICKS {
		if let Some(video) = video.as_mut() {
			let keyframe = tick % GOP == 0;
			let slice = if keyframe {
				vec![0x65u8; 3_000]
			} else {
				vec![0x41u8; 400]
			};
			video
				.write(Frame {
					timestamp: Timestamp::from_micros(tick * VIDEO_US).unwrap(),
					duration: None,
					payload: length_prefixed(&[&slice]),
					keyframe,
				})
				.unwrap();
		}
		// Every audio frame that starts before the next video tick.
		while audio_index * AUDIO_US < (tick + 1) * VIDEO_US {
			audio
				.write(Frame {
					timestamp: Timestamp::from_micros(audio_index * AUDIO_US).unwrap(),
					duration: None,
					payload: Bytes::from_iter((0..180u16).map(|i| (i ^ audio_index as u16) as u8)),
					keyframe: audio_index % AUDIO_GROUP == 0,
				})
				.unwrap();
			audio_index += 1;
		}

		out_a.extend(drain_frames(&mut a).await);
		if let Some(b) = b.as_mut() {
			out_b.extend(drain_frames(b).await);
		}
		if tick + 1 == JOIN {
			b = Some(Export::new(source.clone()).await.unwrap());
		}
	}

	(out_a, out_b)
}

/// Pull every frame an exporter can render right now, like `drain` but keeping the frames
/// whole (this compares them one by one, not as one byte stream).
async fn drain_frames<E: tscat::Catalog>(export: &mut Export<E>) -> Vec<Frame> {
	let mut out = Vec::new();
	while let Ok(res) = tokio::time::timeout(Duration::from_secs(1), export.next()).await {
		match res.expect("exporter error") {
			Some(frame) => out.push(frame),
			None => break,
		}
	}
	out
}

/// Compare the overlapping output of two exporters, starting at `from`, and assert the only
/// bytes that disagree are continuity counters.
///
/// The counter is the known exception: it is numbered from process state, so two legs are
/// offset by a constant. Fixing that needs the emitted packet count per group to be a function
/// of the broadcast, which is a much larger change than this guards. Everything else has to
/// match exactly, so this fails if any new field starts being minted per process.
fn assert_only_continuity_differs(a: &[Frame], b: &[Frame], from: Timestamp) {
	let a: Vec<&Frame> = a.iter().filter(|f| f.timestamp >= from).collect();
	let b: Vec<&Frame> = b.iter().filter(|f| f.timestamp >= from).collect();
	assert!(b.len() > 20, "not enough overlap to be worth comparing: {}", b.len());
	assert_eq!(a.len(), b.len(), "exporters rendered a different number of frames");

	for (a, b) in a.iter().zip(b.iter()) {
		assert_eq!(a.timestamp, b.timestamp, "compared frames must be the same frame");
		assert_eq!(
			a.payload.len(),
			b.payload.len(),
			"same frame rendered to a different size"
		);
		assert_packet_aligned(&a.payload);

		for (offset, (x, y)) in a.payload.iter().zip(b.payload.iter()).enumerate() {
			if x == y {
				continue;
			}
			// Byte 3 of a TS packet is `transport_scrambling_control | adaptation_field_control |
			// continuity_counter`, and only the low nibble is the counter. A difference anywhere
			// else is a value the exporter minted from its own state rather than from the broadcast.
			assert_eq!(
				(offset % 188, (x ^ y) & 0xf0),
				(3, 0),
				"frame at {:?} differs outside the continuity counter: offset {offset}, {x:#04x} vs {y:#04x}",
				a.timestamp,
			);
		}
	}
}

#[tokio::test(start_paused = true)]
async fn late_join_matches_a_running_exporter() {
	let (a, b) = export_twice(true).await;

	// Skip to the joiner's second keyframe: its first group covers tune-in, where the two legs
	// legitimately differ because only the joiner has to lead with the program tables.
	let keyframes: Vec<Timestamp> = b.iter().filter(|f| f.keyframe).map(|f| f.timestamp).collect();
	assert_only_continuity_differs(&a, &b, keyframes[1]);
}

/// The same property for a program with no video track. Worth its own case because the program
/// tables are re-emitted at every video keyframe, a boundary both legs share, which hides a
/// drifting cadence. Audio-only has no such boundary, so the cadence has to come from the media
/// timeline on its own.
#[tokio::test(start_paused = true)]
async fn late_join_matches_a_running_exporter_without_video() {
	let (a, b) = export_twice(false).await;

	// The joiner leads with its own PCR and a PAT/PMT-carrying tune-in frame so a receiver
	// can start; the running exporter has no reason to repeat those there. From the first
	// timestamp after the tune-in frame the two are rendering the same stream. (The joiner's
	// leading PCR is stamped at a slot boundary at or before the tune-in frame, so comparing
	// strictly after the tune-in excludes both.)
	let tune_in = b.iter().find(|f| !is_pcr_frame(f)).expect("a tune-in frame").timestamp;
	let from = b.iter().map(|f| f.timestamp).filter(|&t| t > tune_in).min().unwrap();
	assert_only_continuity_differs(&a, &b, from);
}

/// Every PCR packet in transport order: its packet index in the stream, its value
/// (90 kHz), and the media timestamp of the frame carrying it.
fn collect_pcrs(frames: &[Frame]) -> Vec<(usize, u64, u128)> {
	let mut at = 0;
	let mut pcrs = Vec::new();
	for frame in frames {
		for packet in frame.payload.chunks(188) {
			// adaptation-field-only packets (adaptation_field_control == 0b10) are the clock.
			if packet[3] & 0x30 == 0x20 && packet[5] & 0x10 != 0 {
				let base = (u64::from(packet[6]) << 25)
					| (u64::from(packet[7]) << 17)
					| (u64::from(packet[8]) << 9)
					| (u64::from(packet[9]) << 1)
					| u64::from(packet[10] >> 7);
				pcrs.push((at, base, frame.timestamp.as_micros()));
			}
			at += 1;
		}
	}
	pcrs
}

/// A 4 s CBR-ish single-rendition H.264 feed at 25 fps, one second per group: the
/// shape of a broadcast contribution capture, and the one #3334 measured.
async fn export_cbr_video() -> Vec<Frame> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(broadcast.unique_name(".h264"), hang::container::track_info())
		.unwrap();
	{
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		cfg.container = Container::Legacy;
		catalog.lock().video.renditions.insert(track.name().to_string(), cfg);
	}
	let mut video = Producer::new(track, HangContainer::Legacy);
	for i in 0..100u64 {
		let keyframe = i % 25 == 0;
		let mut nal = vec![if keyframe { 0x65u8 } else { 0x41 }];
		nal.extend(std::iter::repeat_n(0xAB, 7_000));
		if keyframe && i > 0 {
			video.cut(None).unwrap();
		}
		video
			.write(Frame {
				timestamp: Timestamp::from_micros(i * 40_000).unwrap(),
				duration: None,
				payload: if keyframe {
					annexb(&[SPS, PPS, &nal])
				} else {
					annexb(&[&nal])
				},
				keyframe,
			})
			.unwrap();
	}
	video.finish().unwrap();

	let mut exporter = Export::new(crate::source::announced(&consumer)).await.unwrap();
	drain_frames(&mut exporter).await
}

/// #3334: the clock a receiver recovers from *byte position* has to agree with the
/// values, because a byte stream carries no other timing. Emitting each media frame
/// whole put every PCR between two frames instead of among the bytes it labels, so
/// the grid #2967 made exact could not be read off the wire: consecutive clock
/// packets sat one packet apart with the media they described heaped between the
/// clusters, and a downstream stage re-deriving PCR from position regenerated the
/// original clustered distribution.
#[tokio::test(start_paused = true)]
async fn pcr_rides_the_bytes_it_labels() {
	let frames = export_cbr_video().await;
	let pcrs = collect_pcrs(&frames);
	assert!(pcrs.len() > 100, "expected the full feed, got {} PCRs", pcrs.len());

	// Every step is one grid slot, so every gap should carry the same span of media
	// and therefore a comparable number of packets.
	let step = PCR_INTERVAL.as_micros() as u64 * 90 / 1_000;
	for (i, w) in pcrs.windows(2).enumerate() {
		assert_eq!(w[1].1.wrapping_sub(w[0].1) & ((1 << 33) - 1), step, "value step at {i}");
	}

	let gaps: Vec<usize> = pcrs.windows(2).map(|w| w[1].0 - w[0].0).collect();
	assert_eq!(
		gaps.iter().filter(|&&gap| gap == 1).count(),
		0,
		"no clock packet may sit adjacent to the previous one: {gaps:?}"
	);
	let mut sorted = gaps.clone();
	sorted.sort_unstable();
	let median = sorted[sorted.len() / 2];
	// The leading group carries the parameter sets and program tables on top of its
	// media, so allow generous headroom; what this rules out is the bimodal
	// distribution (one packet, then hundreds) that made the clock unrecoverable.
	assert!(
		sorted[0] * 3 >= median && sorted[sorted.len() - 1] <= median * 3,
		"packet gaps must track the interval the values assert, got {sorted:?}"
	);
}

/// The other half of #3334: a PCR's *release* has to track the interval its own
/// value asserts. The exporter only stamps; the caller paces on the stamps (see
/// [`moq_mux::Pacer`]), so the property here is that consecutive clock packets are
/// stamped exactly one grid interval apart. Frames are emitted whole and a slot's
/// bytes are only all in hand once media past it has arrived, so a stamp that
/// tracked frame arrival was already in the past and its sleep was a no-op.
#[tokio::test(start_paused = true)]
async fn pcr_stamps_step_by_the_grid() {
	let frames = export_cbr_video().await;
	let pcrs = collect_pcrs(&frames);

	let steps: Vec<i128> = pcrs.windows(2).map(|w| w[1].2 as i128 - w[0].2 as i128).collect();
	let interval = PCR_INTERVAL.as_micros() as i128;
	assert!(
		steps.iter().all(|&step| step == interval),
		"every clock packet must be stamped one grid interval past the last: {steps:?}"
	);
}

/// The same positional property on a real reordered (B-frame) capture with a second
/// rendition, where the exporter has no uniform cadence to lean on: frames arrive in
/// decode order, and the two tracks advance the media clock at different rates. The
/// clock still lands among the bytes it labels rather than clustering, though the
/// gaps are no longer uniform: a span is however long it took the next timestamp to
/// arrive, which for interleaved tracks is nothing like the media a frame's bytes
/// represent. Evening that out needs the muxer to hold a byte buffer and drain it at
/// a measured rate, which this does not do.
#[tokio::test(start_paused = true)]
async fn pcr_stays_among_the_bytes_across_reordered_tracks() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	let mut exporter = Export::new(crate::source::announced(&consumer)).await.unwrap();
	let frames = drain_frames(&mut exporter).await;
	let pcrs = collect_pcrs(&frames);
	assert!(pcrs.len() > 50, "expected the full feed, got {} PCRs", pcrs.len());

	let gaps: Vec<usize> = pcrs.windows(2).map(|w| w[1].0 - w[0].0).collect();
	assert_eq!(
		gaps.iter().filter(|&&gap| gap == 1).count(),
		0,
		"no clock packet may sit adjacent to the previous one: {gaps:?}"
	);

	// Stamps still step by exactly one slot, apart from the first interval, which
	// covers the leading span rather than a whole slot.
	let interval = PCR_INTERVAL.as_micros() as i128;
	let off = pcrs
		.windows(2)
		.filter(|w| w[1].2 as i128 - w[0].2 as i128 != interval)
		.count();
	assert!(off <= 1, "{off} clock packets are stamped off the grid");
}

/// A clock packet carries no payload, so ISO 13818-1 2.4.3.3 says it must repeat
/// the continuity counter of whatever preceded it on its PID rather than advance
/// it. The clock rides a PID that also carries media, and slicing on the grid puts
/// clock packets *inside* a frame's packet run, whose counters were assigned when
/// the frame was muxed rather than when the bytes go out. Numbering the clock
/// packet from the counter's current value there lands it a whole frame ahead, and
/// an analyzer reports a discontinuity on it and another on the payload packet
/// after it.
#[tokio::test(start_paused = true)]
async fn payload_less_clock_packets_repeat_the_counter() {
	let frames = export_cbr_video().await;
	let ts: Vec<u8> = frames.iter().flat_map(|f| f.payload.iter().copied()).collect();
	assert_packet_aligned(&ts);

	let mut last: std::collections::HashMap<u16, u8> = std::collections::HashMap::new();
	let mut advanced = 0;
	let mut discontinuities = 0;
	for (i, packet) in ts.chunks(188).enumerate() {
		let pid = u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2]);
		let cc = packet[3] & 0x0f;
		let payload = packet[3] & 0x10 != 0;
		if let Some(&prev) = last.get(&pid) {
			if payload && cc != (prev + 1) & 0x0f {
				discontinuities += 1;
				eprintln!("discontinuity at packet {i} on pid {pid}: {prev} -> {cc}");
			} else if !payload && cc != prev {
				advanced += 1;
				eprintln!("payload-less packet {i} on pid {pid} advanced: {prev} -> {cc}");
			}
		}
		last.insert(pid, cc);
	}
	assert_eq!(advanced, 0, "payload-less packets must repeat the counter");
	assert_eq!(discontinuities, 0, "the counter must be continuous on every PID");
}
