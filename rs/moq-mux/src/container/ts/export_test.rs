//! Tests for the MPEG-TS exporter.
//!
//! AAC audio is framed as ADTS (MP2/AC-3 pass through as whole frames); video
//! is normalized to length-prefixed NALU by
//! `ExportSource` and rewritten to Annex-B by the muxer (re-injecting the
//! parameter sets on keyframes). These build a synthetic broadcast, export to
//! TS, and re-parse with the `mpeg2ts` reader.

use std::io::Cursor;

use bytes::{Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, VideoConfig};
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::{ReadTsPacket, TsPacketReader, TsPayload};

use crate::catalog::hang::Container as HangContainer;
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
async fn drain(consumer: moq_net::BroadcastConsumer) -> BytesMut {
	drain_with(Export::new(consumer).await.unwrap()).await
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(
			broadcast.unique_name(".aac"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
		producer.finish_group().unwrap();
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	// In-band avc3 video (SPS/PPS inline on keyframes; no out-of-band description).
	let vtrack = broadcast
		.create_track(
			broadcast.unique_name(".avc3"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
		.create_track(
			broadcast.unique_name(".aac"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
		audio.finish_group().unwrap();
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
	video.finish_group().unwrap();
	for ms in [100, 120, 140] {
		audio.write(audio_frame(ms)).unwrap();
		audio.finish_group().unwrap();
	}
	video.finish().unwrap();
	audio.finish().unwrap();

	let exporter = Export::new(consumer).await.unwrap();
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
				// The first packet of a keyframe must signal random access and carry a PCR.
				if let Some(af) = &packet.adaptation_field {
					saw_random_access |= af.random_access_indicator;
					saw_pcr |= af.pcr.is_some();
				}
				unbounded = pes.pes_packet_len == 0;
				reassembled.extend_from_slice(&pes.data);
			}
			Some(TsPayload::PesContinuation(bytes)) => reassembled.extend_from_slice(&bytes),
			_ => {}
		}
	}

	assert!(video_pid.is_some(), "missing video PMT entry");
	assert!(saw_random_access, "keyframe should set random_access_indicator");
	assert!(saw_pcr, "PCR pid should carry a PCR on the keyframe");
	assert!(unbounded, "video PES should be unbounded");
	reassembled
}

/// In-band avc3: SPS/PPS are inline in the bitstream. ExportSource strips them
/// into a synthesized avcC, and the muxer re-injects them on the keyframe.
#[tokio::test(start_paused = true)]
async fn export_avc3_in_band_reassembles() {
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(
			broadcast.unique_name(".avc3"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(
			broadcast.unique_name(".avc3"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap();

	let track = broadcast
		.create_track(
			broadcast.unique_name(".avc1"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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

/// A real broadcast contribution feed (Ateme Kyrion, H.264 1080i with ~86 B-frames)
/// must come out of the exporter with an authored decode timeline. The importer publishes
/// the reorder depth as the catalog `jitter`, and the exporter sizes its decode-clock reserve
/// from it, so the video PES carry a DTS that is both strictly increasing and never after the
/// PTS in decode order. Also assert the reorder was real (non-monotonic PTS in the source).
#[tokio::test(start_paused = true)]
async fn export_bframe_video_authors_dts() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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
			Some(TsPayload::Pmt(pmt)) => {
				if video_pid.is_none() {
					video_pid = pmt
						.es_info
						.iter()
						.find(|e| e.stream_type == StreamType::H264)
						.map(|e| e.elementary_pid);
				}
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

/// Full SCTE-35 round-trip: import `bbb.ts` (real H.264 + AAC) into a broadcast
/// that also carries a `.scte35` cue track, export to TS, re-import, and assert
/// the splice_info_section came back byte-for-byte. The PMT must advertise the
/// SCTE-35 stream (0x86) and the program-level CUEI registration descriptor.
#[tokio::test(start_paused = true)]
async fn export_scte35_roundtrip() {
	let data = include_bytes!("test_data/bbb.ts");

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// Create and write the SCTE-35 cue track BEFORE moving `broadcast` into
	// `Import` (which consumes it); the producer stays alive so the exporter can
	// subscribe to the retained track.
	let scte = broadcast
		.unique_track(
			".scte35",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
	scte_producer.finish_group().unwrap();
	scte_producer.finish().unwrap();

	// Now add the real video/audio by importing bbb.ts (this moves `broadcast`).
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	// `import`, `catalog`, and `scte_producer` stay alive: retained tracks. The
	// exporter must carry the extension to see the mpegts section.
	let ts = drain_with(
		Export::with_ts(consumer, crate::catalog::CatalogFormat::Hang)
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
	let mut broadcast2 = moq_net::BroadcastInfo::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 =
		crate::catalog::Producer::with_catalog(&mut broadcast2, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.clone());
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// Build the verbatim PES track BEFORE moving `broadcast` into `Import`; the
	// producer stays alive so the exporter can subscribe to the retained track.
	let data_track = broadcast
		.unique_track(
			".data",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
		.unwrap();
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
	data_producer.finish_group().unwrap();
	data_producer.finish().unwrap();

	// Real video/audio supplies the media clock (moves `broadcast`).
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	// `import`, `catalog`, and `data_producer` stay alive: retained tracks.
	let ts = drain_with(
		Export::with_ts(consumer, crate::catalog::CatalogFormat::Hang)
			.await
			.unwrap(),
	)
	.await;
	assert_packet_aligned(&ts);

	// Re-import the exported TS and recover the verbatim PES stream.
	let mut broadcast2 = moq_net::BroadcastInfo::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 =
		crate::catalog::Producer::with_catalog(&mut broadcast2, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.clone());
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// A SCTE-35 cue track and nothing else.
	let scte = broadcast
		.unique_track(
			".scte35",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
	producer.finish_group().unwrap();
	producer.finish().unwrap();

	let mut exporter = Export::with_ts(consumer, crate::catalog::CatalogFormat::Hang)
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
async fn read_frames(consumer: &moq_net::BroadcastConsumer, name: &str) -> Vec<Vec<u8>> {
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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

	let mut broadcast2 = moq_net::BroadcastInfo::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.clone());
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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

	let mut broadcast2 = moq_net::BroadcastInfo::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.clone());
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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

	let mut broadcast2 = moq_net::BroadcastInfo::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.clone());
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
	consumer: &moq_net::BroadcastConsumer,
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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

	let mut broadcast2 = moq_net::BroadcastInfo::new().produce();
	let consumer2 = broadcast2.consume();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.clone());
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
async fn read_cues(consumer: &moq_net::BroadcastConsumer, name: &str) -> Vec<(Vec<u8>, Timestamp)> {
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
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(
			&mut broadcast,
			crate::catalog::hang::Catalog::<tscat::Ext>::default(),
		)
		.unwrap();
		let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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
			Export::with_ts(consumer, crate::catalog::CatalogFormat::Hang)
				.await
				.unwrap(),
		)
		.await;
		assert_packet_aligned(&ts);

		let mut broadcast2 = moq_net::BroadcastInfo::new().produce();
		let consumer2 = broadcast2.consume();
		let catalog2 = crate::catalog::Producer::with_catalog(
			&mut broadcast2,
			crate::catalog::hang::Catalog::<tscat::Ext>::default(),
		)
		.unwrap();
		let mut import2 = crate::container::ts::Import::new(broadcast2, catalog2.clone());
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(
			broadcast.unique_name(".opus"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
		producer.finish_group().unwrap();
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast
		.create_track(
			broadcast.unique_name(".opus"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
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
		producer.finish_group().unwrap();
	}
	producer.finish().unwrap();

	let ts = drain(consumer).await;

	// Re-import the TS we just produced.
	let mut imported = moq_net::BroadcastInfo::new().produce();
	let imported_consumer = imported.consume();
	let import_catalog = crate::catalog::Producer::new(&mut imported).unwrap();
	let mut import = crate::container::ts::Import::new(imported, import_catalog.clone());
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
