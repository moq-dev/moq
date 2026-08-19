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
/// A drift budget no test timeline comes close to, so the exporter reads every group.
///
/// The media track's full retention window, so an exporter started after publishing
/// can still read every retained group. These tests write a whole broadcast up front
/// and only then export it, which the
/// exporter's default [`Latency::REAL_TIME`](crate::Latency::REAL_TIME) collapses to the
/// live edge: completeness has to be asked for, exactly as a real recorder does.
const RECORDING_LATENCY: std::time::Duration = std::time::Duration::from_secs(30);

async fn drain(consumer: moq_net::broadcast::Consumer) -> BytesMut {
	drain_with(Export::new(crate::source::announced(&consumer)).await.unwrap()).await
}

/// `drain` for an exporter built with an explicit catalog extension.
async fn drain_with<E: tscat::Catalog>(exporter: Export<E>) -> BytesMut {
	let mut exporter = exporter.with_latency(crate::Latency::max(RECORDING_LATENCY));
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

	let track = consumer2
		.track(&name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_latency(crate::Latency::max(RECORDING_LATENCY)))
		.await
		.unwrap();
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

	let track = consumer2
		.track(&name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_latency(crate::Latency::max(RECORDING_LATENCY)))
		.await
		.unwrap();
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
	let track = consumer
		.track(name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_latency(crate::Latency::max(RECORDING_LATENCY)))
		.await
		.unwrap();
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
	let track = consumer
		.track(name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_latency(crate::Latency::max(RECORDING_LATENCY)))
		.await
		.unwrap();
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

/// Build a well-formed long-form section: the full generic header (extension,
/// current version, `number` of `last`), then `body` and a CRC placeholder
/// (capture is verbatim, nothing checks it). The SI store buffers a sub-table
/// until its generation completes, so the header fields must be coherent.
fn make_long_section(table_id: u8, ext: u16, version: u8, number: u8, last: u8, body: &[u8]) -> Vec<u8> {
	let section_length = 5 + body.len() + 4;
	let mut s = vec![
		table_id,
		0xb0 | ((section_length >> 8) as u8 & 0x0f),
		(section_length & 0xff) as u8,
		(ext >> 8) as u8,
		(ext & 0xff) as u8,
		0xc0 | (version << 1) | 0x01,
		number,
		last,
	];
	s.extend_from_slice(body);
	s.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
	s
}

/// Read an SI snapshot track from the start: every group, as its frames' payloads.
async fn read_si_groups(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<Vec<Bytes>> {
	let mut track = consumer
		.track(name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_start(moq_net::track::Position::group(0)))
		.await
		.unwrap();
	let mut groups = Vec::new();
	while let Some(mut group) = track.recv_group().await.unwrap() {
		let mut frames = Vec::new();
		while let Some(frame) = kio::wait(|waiter| group.poll_read_frame(waiter)).await.unwrap() {
			frames.push(frame.payload);
		}
		groups.push(frames);
	}
	groups
}

/// The newest snapshot of an SI track, reduced to its sections.
async fn read_si_sections(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<Bytes> {
	let groups = read_si_groups(consumer, name).await;
	let frames = groups.last().expect("at least one snapshot group");
	frames
		.iter()
		.flat_map(crate::container::ts::si::split_sections)
		.collect()
}

/// Wrap a complete section in one PUSI TS packet on `pid` (pointer_field 0), padded to 188.
fn si_packet(pid: u16, section: &[u8]) -> Vec<u8> {
	si_packet_cc(pid, section, 0)
}

/// [`si_packet`] with an explicit continuity counter. A repetition must vary the
/// counter to reach the SI store at all: a byte-identical packet is dropped as a TS
/// duplicate before reassembly.
fn si_packet_cc(pid: u16, section: &[u8], cc: u8) -> Vec<u8> {
	let mut p = vec![
		0x47,
		0x40 | ((pid >> 8) as u8 & 0x1f),
		(pid & 0xff) as u8,
		0x10 | (cc & 0x0f),
		0x00,
	];
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
	let nit = make_long_section(0x40, 0x1234, 1, 0, 0, &[0xff, 0x01]);

	// Prepend a synthetic NIT Actual packet (0x0010); prepend keeps bbb's alignment.
	// Twice, because real SI repeats every few seconds: the repetition must collapse
	// into the same single committed section rather than cutting a second group. The
	// repeat varies the continuity counter so it reaches the store (an identical
	// packet would be dropped as a TS duplicate before reassembly).
	let mut input = si_packet(0x0010, &nit);
	input.extend_from_slice(&si_packet_cc(0x0010, &nit, 1));
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
	let entry = |pid: u16, table_id: u8| {
		si.get(&pid)
			.and_then(|tables| tables.get(&table_id))
			.expect("an SI entry for the (PID, table_id)")
	};

	let sdt_entry = entry(0x0011, 0x42);
	assert_eq!(
		sdt_entry.interval,
		Some(std::time::Duration::from_secs(2)),
		"the DVB SDT interval was filled in"
	);
	let sdt_sections = read_si_sections(&consumer, &sdt_entry.track).await;
	assert_eq!(sdt_sections.len(), 1, "bbb.ts carries one SDT section");
	let sdt = sdt_sections[0].clone();
	assert_eq!(sdt.first(), Some(&0x42), "SDT Actual (table_id 0x42)");
	let (service_type, provider, name) = parse_sdt_service(&sdt);
	assert_eq!(
		(service_type, provider.as_str(), name.as_str()),
		(0x01, "FFmpeg", "Service01")
	);

	let nit_entry = entry(0x0010, 0x40);
	assert_eq!(
		nit_entry.interval,
		Some(std::time::Duration::from_secs(10)),
		"the DVB NIT interval was filled in"
	);
	let nit_groups = read_si_groups(&consumer, &nit_entry.track).await;
	assert_eq!(
		nit_groups.len(),
		1,
		"the repeated NIT committed once; a repetition cuts no group"
	);
	assert_eq!(
		nit_groups[0],
		vec![Bytes::from(nit.clone())],
		"the NIT snapshot is the section, byte-for-byte"
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
	let consumer2 = broadcast2.consume();
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
	assert_eq!(snapshot2.mpegts.si, si, "every SI entry survived the round-trip");
	assert_eq!(
		read_si_sections(&consumer2, &snapshot2.mpegts.si[&0x0011][&0x42].track).await,
		vec![sdt],
		"the SDT survived byte-for-byte"
	);
	assert_eq!(
		read_si_sections(&consumer2, &snapshot2.mpegts.si[&0x0010][&0x40].track).await,
		vec![Bytes::from(nit)],
		"the NIT survived byte-for-byte"
	);
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

	// SI snapshot tracks, one group each: an SDT (2s) and a NIT (10s).
	let mut sdt_track = broadcast.create_track("0x0011-0x42.si", None).unwrap();
	sdt_track
		.write_frame(
			Timestamp::ZERO,
			Bytes::from(make_long_section(0x42, 1, 0, 0, 0, &[0xaa; 8])),
		)
		.unwrap();
	let mut nit_track = broadcast.create_track("0x0010-0x40.si", None).unwrap();
	nit_track
		.write_frame(
			Timestamp::ZERO,
			Bytes::from(make_long_section(0x40, 1, 0, 0, 0, &[0xbb; 8])),
		)
		.unwrap();

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
		guard.mpegts.si.entry(0x0011).or_default().insert(
			0x42,
			tscat::SiEntry {
				track: "0x0011-0x42.si".to_string(),
				interval: Some(Duration::from_secs(2)),
				..Default::default()
			},
		);
		guard.mpegts.si.entry(0x0010).or_default().insert(
			0x40,
			tscat::SiEntry {
				track: "0x0010-0x40.si".to_string(),
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

/// A DVB SI section larger than one TS packet must be reassembled and captured verbatim.
/// `si_packet` only covers a single-packet section; a real SDT with several services (or a
/// NIT) spans packets, exercising the `SectionReassembler` PUSI + continuity path that
/// feeds the SI store. The body is arbitrary here (capture is verbatim, not parsed).
#[tokio::test(start_paused = true)]
async fn multi_packet_si_section_is_captured() {
	// 400-byte body forces the SDT Actual across three TS packets (183 + 184 + rest).
	let body: Vec<u8> = (0..400u16).map(|i| i as u8).collect();
	let sdt = make_long_section(0x42, 1, 0, 0, 0, &body);
	let input = si_packets_multi(0x0011, &sdt);
	assert!(input.len() > 188, "the SDT must span more than one TS packet");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&input[..])).unwrap();
	import.finish().unwrap();

	let si = catalog.snapshot().mpegts.si.clone();
	let entry = si
		.get(&0x0011)
		.and_then(|tables| tables.get(&0x42))
		.expect("an SDT entry");
	assert_eq!(
		read_si_sections(&consumer, &entry.track).await,
		vec![Bytes::from(sdt)],
		"the multi-packet SDT was reassembled and captured byte-for-byte"
	);
}

/// Import rig for SI-only fixtures: importer, catalog, and a consumer, all kept
/// alive so the SI tracks stay readable after `finish`.
struct SiRig {
	import: crate::container::ts::Import<tscat::Ext>,
	catalog: crate::catalog::Producer<tscat::Ext>,
	consumer: moq_net::broadcast::Consumer,
}

fn si_rig() -> SiRig {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	SiRig {
		import,
		catalog,
		consumer,
	}
}

/// #2881: a multi-section table revised one section at a time must never publish a
/// torn set. The store buffers the incoming generation and commits it atomically,
/// so every snapshot group holds a single version.
#[tokio::test(start_paused = true)]
async fn torn_transition_is_never_published() {
	let sdt = |version: u8, number: u8, fill: u8| make_long_section(0x42, 1, version, number, 1, &[fill; 4]);
	let mut rig = si_rig();

	// Version 5 arrives complete in one batch.
	let mut input = si_packet(0x0011, &sdt(5, 0, 0xaa));
	input.extend_from_slice(&si_packet_cc(0x0011, &sdt(5, 1, 0xab), 1));
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	// Version 6 arrives torn across two batches; the intermediate state (section 0
	// at v6, section 1 still v5) must never hit the track.
	rig.import
		.decode(&BytesMut::from(&si_packet_cc(0x0011, &sdt(6, 0, 0xba), 2)[..]))
		.unwrap();
	rig.import
		.decode(&BytesMut::from(&si_packet_cc(0x0011, &sdt(6, 1, 0xbb), 3)[..]))
		.unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	let track = &si[&0x0011][&0x42].track;
	let groups = read_si_groups(&rig.consumer, track).await;
	assert!(!groups.is_empty(), "at least one snapshot group");
	for (i, frames) in groups.iter().enumerate() {
		let versions: Vec<u8> = frames
			.iter()
			.flat_map(crate::container::ts::si::split_sections)
			.map(|section| (section[5] >> 1) & 0x1f)
			.collect();
		assert!(
			versions.windows(2).all(|w| w[0] == w[1]),
			"group {i} mixes versions: {versions:?}"
		);
	}
	assert_eq!(
		read_si_sections(&rig.consumer, track).await,
		vec![Bytes::from(sdt(6, 0, 0xba)), Bytes::from(sdt(6, 1, 0xbb))],
		"the newest snapshot is version 6, complete"
	);
}

/// EIT rides per-table entries (#2800): now/next (0x4E) and schedule (0x50) each
/// get their own track and their own cadence, and the schedule's deliberately
/// sparse section numbering (segments skip unused numbers) commits on cycle wrap
/// rather than waiting for a contiguity that never comes.
#[tokio::test(start_paused = true)]
async fn eit_now_next_and_schedule_are_captured() {
	// Body layout past the generic header: TSID(2), ONID(2), then filler.
	let pf0 = make_long_section(0x4E, 1, 0, 0, 1, &[0x00, 0x01, 0x00, 0x02, 0x01, 0x4E, 0xaa]);
	let pf1 = make_long_section(0x4E, 1, 0, 1, 1, &[0x00, 0x01, 0x00, 0x02, 0x01, 0x4E, 0xbb]);
	let sc0 = make_long_section(0x50, 1, 0, 0, 8, &[0x00, 0x01, 0x00, 0x02, 0x08, 0x50, 0xcc]);
	let sc8 = make_long_section(0x50, 1, 0, 8, 8, &[0x00, 0x01, 0x00, 0x02, 0x08, 0x50, 0xdd]);

	let mut input = Vec::new();
	for (cc, section) in [&pf0, &pf1, &sc0, &sc8, &sc0].into_iter().enumerate() {
		input.extend_from_slice(&si_packet_cc(0x0012, section, cc as u8));
	}
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	let eit = si.get(&0x0012).expect("EIT entries");

	let pf = eit.get(&0x4E).expect("a now/next entry");
	assert_eq!(pf.interval, Some(Duration::from_secs(2)), "now/next actual: 2s");
	assert_eq!(
		read_si_sections(&rig.consumer, &pf.track).await,
		vec![Bytes::from(pf0), Bytes::from(pf1)],
		"now/next committed on contiguity"
	);

	let sched = eit.get(&0x50).expect("a schedule entry");
	assert_eq!(sched.interval, Some(Duration::from_secs(10)), "schedule actual: 10s");
	assert_eq!(
		read_si_sections(&rig.consumer, &sched.track).await,
		vec![Bytes::from(sc0), Bytes::from(sc8)],
		"the sparse schedule committed on cycle wrap"
	);
}

/// #2842: SDT other sections from two networks that reuse a transport_stream_id
/// must not collide. The identity reads original_network_id (bytes 8..10) for
/// table_id 0x46, so both survive as separate sub-tables; a revision within one
/// network still replaces in place.
#[tokio::test(start_paused = true)]
async fn sdt_other_networks_do_not_collide() {
	// Same TSID (the extension), different ONID leading the body.
	let net1 = make_long_section(0x46, 7, 0, 0, 0, &[0x00, 0x01, 0xaa]);
	let net2 = make_long_section(0x46, 7, 0, 0, 0, &[0x00, 0x02, 0xbb]);
	let net1v2 = make_long_section(0x46, 7, 1, 0, 0, &[0x00, 0x01, 0xcc]);

	let mut input = si_packet(0x0011, &net1);
	input.extend_from_slice(&si_packet_cc(0x0011, &net2, 1));
	input.extend_from_slice(&si_packet_cc(0x0011, &net1v2, 2));
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	let entry = &si[&0x0011][&0x46];
	assert_eq!(
		read_si_sections(&rig.consumer, &entry.track).await,
		vec![Bytes::from(net1v2), Bytes::from(net2)],
		"both networks survive; the revision replaced only its own network"
	);
}

/// A next-version section (current_next_indicator clear) describes a future state
/// and is dropped: only what is currently in force is carried. The current NIT is
/// the positive control proving the pipeline ran; a lone dropped packet would pass
/// vacuously, since sync lock needs a second packet before anything routes.
#[tokio::test(start_paused = true)]
async fn next_version_sections_are_dropped() {
	let mut next = make_long_section(0x42, 1, 3, 0, 0, &[0xaa; 4]);
	next[5] &= !0x01;
	let nit = make_long_section(0x40, 1, 0, 0, 0, &[0xbb; 4]);

	let mut input = si_packet(0x0011, &next);
	input.extend_from_slice(&si_packet_cc(0x0011, &next, 1));
	input.extend_from_slice(&si_packet(0x0010, &nit));
	input.extend_from_slice(&si_packet_cc(0x0010, &nit, 1));
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	assert!(si.contains_key(&0x0010), "the current NIT was captured (control)");
	assert!(!si.contains_key(&0x0011), "a next-version section creates no entry");
}

/// TDT/TOT (0x0014) stays out by policy: it is a clock, not state, so every
/// section is new content, and an exporter's own clock beats a time relayed from
/// an upstream multiplexer of unknown delay. The SDT is the positive control
/// proving the pipeline ran.
#[tokio::test(start_paused = true)]
async fn tdt_is_not_captured() {
	// A short-form TDT: table_id 0x70, no long-form header.
	let tdt = make_short_section(0x70, &[0xc0, 0x79, 0x12, 0x34, 0x56]);
	let sdt = make_long_section(0x42, 1, 0, 0, 0, &[0xaa; 4]);

	let mut input = si_packet(0x0014, &tdt);
	input.extend_from_slice(&si_packet_cc(0x0014, &tdt, 1));
	input.extend_from_slice(&si_packet(0x0011, &sdt));
	input.extend_from_slice(&si_packet_cc(0x0011, &sdt, 1));
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	assert!(si.contains_key(&0x0011), "the SDT was captured (control)");
	assert!(
		!si.contains_key(&0x0014),
		"TDT is not captured; the omission is policy, not oversight"
	);
}

/// Build a well-formed short-form section (syntax indicator clear): header + body,
/// no extension, versioning, or CRC.
fn make_short_section(table_id: u8, body: &[u8]) -> Vec<u8> {
	let mut s = vec![
		table_id,
		0x30 | ((body.len() >> 8) as u8 & 0x0f),
		(body.len() & 0xff) as u8,
	];
	s.extend_from_slice(body);
	s
}

/// Aborting the importer must remove the advertised SI entries from the catalog:
/// a map naming aborted tracks would strand every later exporter on subscriptions
/// that can never deliver.
#[tokio::test(start_paused = true)]
async fn abort_removes_si_catalog_entries() {
	let sdt = make_long_section(0x42, 1, 0, 0, 0, &[0xaa; 4]);
	// Twice: sync lock needs a second packet before the first routes at all.
	let mut input = si_packet(0x0011, &sdt);
	input.extend_from_slice(&si_packet_cc(0x0011, &sdt, 1));
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	assert!(
		!rig.catalog.snapshot().mpegts.si.is_empty(),
		"the SDT entry was advertised"
	);

	rig.import.abort(moq_net::Error::Cancel);
	assert!(
		rig.catalog.snapshot().mpegts.si.is_empty(),
		"abort removed the advertised entries"
	);
}

/// Content-identified sections (short-form: no version, nothing to revise in
/// place) are retained up to a cap, and churn at the cap is not a change: a
/// clock-like table whose every repetition differs would otherwise cut a group
/// per debounce forever, carrying the cap's worth of stale sections.
#[tokio::test(start_paused = true)]
async fn content_section_churn_at_the_cap_cuts_no_group() {
	let section = |i: u8| make_short_section(0x72, &[i, 0x00, 0xee]);

	// One over the cap in one batch: the 33rd evicts the 1st.
	let mut input = Vec::new();
	for i in 0..33u8 {
		input.extend_from_slice(&si_packet_cc(0x0011, &section(i), i & 0x0f));
	}
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	// Another distinct section, pure rotation at the cap: must not re-dirty.
	rig.import
		.decode(&BytesMut::from(&si_packet_cc(0x0011, &section(33), 1)[..]))
		.unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	let groups = read_si_groups(&rig.consumer, &si[&0x0011][&0x72].track).await;
	assert_eq!(groups.len(), 1, "rotation at the cap cut no further group");
	assert_eq!(groups[0].len(), 32, "the snapshot holds the cap's worth of sections");
}

/// SI never gates output: an advertised entry whose track resolves but never
/// delivers a snapshot (a stale announce) must not hold the programme dark.
/// Media flows immediately and the entry simply emits nothing.
#[tokio::test(start_paused = true)]
async fn stale_si_entry_does_not_block_output() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// A track that exists (the subscription resolves) but never produces a group.
	let ghost = broadcast.create_track("ghost.si", None).unwrap();

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
		guard.mpegts.si.entry(0x0011).or_default().insert(
			0x42,
			tscat::SiEntry {
				track: "ghost.si".to_string(),
				interval: Some(Duration::from_secs(2)),
				..Default::default()
			},
		);
	}

	let mut producer = Producer::new(track, HangContainer::Legacy);
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 64));
	producer
		.write(Frame {
			timestamp: Timestamp::ZERO,
			duration: None,
			payload: length_prefixed(&[&idr]),
			keyframe: true,
		})
		.unwrap();
	producer.cut(None).unwrap();
	producer.finish().unwrap();

	let mut exporter = Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	// The timeout distinguishes "produced output promptly" from "held dark";
	// under paused time a wedged exporter would hit it instantly.
	let frame = tokio::time::timeout(Duration::from_secs(1), exporter.next())
		.await
		.expect("a stale SI entry must not hold output dark")
		.unwrap()
		.expect("a muxed frame");
	assert!(!frame.payload.is_empty());
	assert!(
		!frame
			.payload
			.chunks_exact(188)
			.any(|p| ((((p[1] & 0x1f) as u16) << 8) | p[2] as u16) == 0x0011),
		"nothing was emitted for the undelivered entry"
	);
	drop(ghost);
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

	// The joiner leads with PAT/PMT on its very first frame so a receiver can tune in; the
	// running exporter has no reason to repeat them there. From the next frame on the two are
	// rendering the same stream.
	assert_only_continuity_differs(&a, &b, b[1].timestamp);
}

/// A section lost before the cycle wraps commits an observed subset; the next
/// cycle must *converge* to the full set rather than flip-flop between subsets.
/// The repetition fast-path skips sections already active, so without the
/// same-version merge in `commit` the stragglers would replace the subset instead
/// of completing it, oscillating forever.
#[tokio::test(start_paused = true)]
async fn lost_dense_section_recovers_on_the_next_cycle() {
	let sdt = |number: u8, fill: u8| make_long_section(0x42, 1, 0, number, 2, &[fill; 4]);
	let s0 = sdt(0, 0xa0);
	let s1 = sdt(1, 0xa1);
	let s2 = sdt(2, 0xa2);

	// Cycle 1 loses section 1; the repeat of section 0 wraps and commits {0, 2}.
	let mut input = si_packet(0x0011, &s0);
	input.extend_from_slice(&si_packet_cc(0x0011, &s2, 1));
	input.extend_from_slice(&si_packet_cc(0x0011, &s0, 2));
	// Cycle 2 supplies section 1; sections 0 and 2 short-circuit as repetitions,
	// so the wrap carries only the straggler, which must merge, not replace.
	input.extend_from_slice(&si_packet_cc(0x0011, &s1, 3));
	input.extend_from_slice(&si_packet_cc(0x0011, &s1, 4));
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	assert_eq!(
		read_si_sections(&rig.consumer, &si[&0x0011][&0x42].track).await,
		vec![Bytes::from(s0), Bytes::from(s1), Bytes::from(s2)],
		"the lost section joined the committed generation instead of replacing it"
	);
}

/// A catalog update that keeps the `(PID, table_id)` key but repoints it at a new
/// track (a restarted publisher) must rebuild the subscription: staying attached
/// to the old track would repeat its stale sections forever.
#[tokio::test(start_paused = true)]
async fn repointed_si_entry_resubscribes() {
	let sdt_a = make_long_section(0x42, 1, 0, 0, 0, &[0xaa; 8]);
	let sdt_b = make_long_section(0x42, 1, 1, 0, 0, &[0xbb; 8]);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	let mut track_a = broadcast.create_track("a.si", None).unwrap();
	track_a
		.write_frame(Timestamp::ZERO, Bytes::from(sdt_a.clone()))
		.unwrap();
	let mut track_b = broadcast.create_track("b.si", None).unwrap();
	track_b
		.write_frame(Timestamp::ZERO, Bytes::from(sdt_b.clone()))
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
		guard.mpegts.si.entry(0x0011).or_default().insert(
			0x42,
			tscat::SiEntry {
				track: "a.si".to_string(),
				interval: Some(Duration::from_secs(2)),
				..Default::default()
			},
		);
	}

	let mut producer = Producer::new(track, HangContainer::Legacy);
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 64));
	let write_key = |producer: &mut Producer<HangContainer>, sec: u64| {
		producer
			.write(Frame {
				timestamp: Timestamp::from_micros(sec * 1_000_000).unwrap(),
				duration: None,
				payload: length_prefixed(&[&idr]),
				keyframe: true,
			})
			.unwrap();
		producer.cut(None).unwrap();
	};

	// Writes both GOPs up front and only then reads, so it needs a replay window:
	// the real-time default would take the live edge and skip the first.
	let mut exporter = Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap()
		.with_latency(crate::Latency::max(RECORDING_LATENCY));
	let mut before = BytesMut::new();
	write_key(&mut producer, 0);
	write_key(&mut producer, 1);
	for _ in 0..2 {
		let frame = tokio::time::timeout(Duration::from_secs(1), exporter.next())
			.await
			.expect("a frame before the switch")
			.unwrap()
			.unwrap();
		before.extend_from_slice(&frame.payload);
	}

	// Repoint the entry at the replacement track.
	catalog.lock().mpegts.si.get_mut(&0x0011).unwrap().insert(
		0x42,
		tscat::SiEntry {
			track: "b.si".to_string(),
			interval: Some(Duration::from_secs(2)),
			..Default::default()
		},
	);

	write_key(&mut producer, 2);
	write_key(&mut producer, 3);
	write_key(&mut producer, 4);
	producer.finish().unwrap();
	let mut after = BytesMut::new();
	while let Ok(res) = tokio::time::timeout(Duration::from_secs(1), exporter.next()).await {
		let Some(frame) = res.unwrap() else { break };
		after.extend_from_slice(&frame.payload);
	}

	let contains = |haystack: &[u8], needle: &[u8]| haystack.windows(needle.len()).any(|w| w == needle);
	assert!(
		contains(&before, &sdt_a),
		"the original track's SDT was emitted (control)"
	);
	assert!(
		contains(&after, &sdt_b),
		"the replacement track's SDT is emitted after the repoint"
	);
}

/// An SI revision that lands after the final media frame still reaches the TS:
/// emission rides media frames, so end of stream flushes every entry's current
/// sections in one trailing frame before yielding `None`.
#[tokio::test(start_paused = true)]
async fn si_revision_after_final_media_frame_is_flushed() {
	let sdt_v1 = make_long_section(0x42, 1, 0, 0, 0, &[0xaa; 8]);
	let sdt_v2 = make_long_section(0x42, 1, 1, 0, 0, &[0xbb; 8]);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	let mut si_track = broadcast.create_track("0x0011-0x42.si", None).unwrap();
	si_track
		.write_frame(Timestamp::ZERO, Bytes::from(sdt_v1.clone()))
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
		guard.mpegts.si.entry(0x0011).or_default().insert(
			0x42,
			tscat::SiEntry {
				track: "0x0011-0x42.si".to_string(),
				interval: Some(Duration::from_secs(2)),
				..Default::default()
			},
		);
	}

	let mut producer = Producer::new(track, HangContainer::Legacy);
	let mut idr = vec![0x65u8];
	idr.extend(std::iter::repeat_n(0xAB, 64));
	producer
		.write(Frame {
			timestamp: Timestamp::ZERO,
			duration: None,
			payload: length_prefixed(&[&idr]),
			keyframe: true,
		})
		.unwrap();
	producer.cut(None).unwrap();
	producer.finish().unwrap();

	let mut exporter = Export::with_ts(crate::source::announced(&consumer), crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let first = tokio::time::timeout(Duration::from_secs(1), exporter.next())
		.await
		.expect("the only media frame")
		.unwrap()
		.unwrap();
	let contains = |haystack: &[u8], needle: &[u8]| haystack.windows(needle.len()).any(|w| w == needle);
	assert!(contains(&first.payload, &sdt_v1), "v1 rode the media frame (control)");

	// The revision lands after the last media frame; only the trailing flush can
	// carry it. Closing the catalog lets the exporter reach end of stream.
	si_track
		.write_frame(Timestamp::ZERO, Bytes::from(sdt_v2.clone()))
		.unwrap();
	catalog.finish().unwrap();

	let tail = tokio::time::timeout(Duration::from_secs(1), exporter.next())
		.await
		.expect("a trailing SI frame rather than an immediate end")
		.unwrap()
		.expect("the trailing SI frame");
	assert_packet_aligned(&tail.payload);
	assert!(
		contains(&tail.payload, &sdt_v2),
		"the trailing flush carries the revision"
	);
	let end = tokio::time::timeout(Duration::from_secs(1), exporter.next())
		.await
		.expect("the stream ends after the flush")
		.unwrap();
	assert!(end.is_none(), "end of stream after the trailing flush");
}

/// Two captures overlapping on one broadcast (a supervisor restarting its
/// importer before the old one is dropped) contend for the same `(PID, table_id)`
/// key: the newer one wins the catalog mapping under a fallback track name, and
/// the older one's teardown must not strip it, since the survivor never
/// re-advertises.
#[tokio::test(start_paused = true)]
async fn overlapping_capture_teardown_keeps_the_survivors_mapping() {
	let sdt = make_long_section(0x42, 1, 0, 0, 0, &[0xaa; 4]);
	let mut input = si_packet(0x0011, &sdt);
	input.extend_from_slice(&si_packet_cc(0x0011, &sdt, 1));

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let _consumer = broadcast.consume();
	let catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();
	let mut old = crate::container::ts::Import::new(broadcast.clone(), catalog.reserve());
	old.decode(&BytesMut::from(&input[..])).unwrap();

	// The replacement importer captures the same table; its deterministic track
	// name is taken, so it advertises under a fallback name, overwriting the key.
	let mut new = crate::container::ts::Import::new(broadcast, catalog.reserve());
	new.decode(&BytesMut::from(&input[..])).unwrap();
	let survivor = catalog.snapshot().mpegts.si[&0x0011][&0x42].track.clone();
	assert_ne!(survivor, "0x0011-0x42.si", "the replacement fell back to a unique name");

	drop(old);
	assert_eq!(
		catalog.snapshot().mpegts.si[&0x0011][&0x42].track,
		survivor,
		"the old capture's teardown left the survivor's mapping in place"
	);
}

/// A contiguous commit replaces even a same-version active generation: versions
/// are five bits, so a reception gap can bring the same value back with fewer
/// sections, and merging would resurrect the removed one forever.
#[tokio::test(start_paused = true)]
async fn contiguous_same_version_commit_replaces_stale_sections() {
	let sdt = |number: u8, last: u8, fill: u8| make_long_section(0x42, 1, 0, number, last, &[fill; 4]);

	// Three sections at v0, committed contiguously.
	let mut input = si_packet(0x0011, &sdt(0, 2, 0xa0));
	input.extend_from_slice(&si_packet_cc(0x0011, &sdt(1, 2, 0xa1), 1));
	input.extend_from_slice(&si_packet_cc(0x0011, &sdt(2, 2, 0xa2), 2));
	// After a gap the table comes back at v0 again (wrapped), now two sections.
	let b0 = sdt(0, 1, 0xb0);
	let b1 = sdt(1, 1, 0xb1);
	input.extend_from_slice(&si_packet_cc(0x0011, &b0, 3));
	input.extend_from_slice(&si_packet_cc(0x0011, &b1, 4));
	let mut rig = si_rig();
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();
	rig.import.finish().unwrap();

	let si = rig.catalog.snapshot().mpegts.si.clone();
	assert_eq!(
		read_si_sections(&rig.consumer, &si[&0x0011][&0x42].track).await,
		vec![Bytes::from(b0), Bytes::from(b1)],
		"the complete new generation retired the stale third section"
	);
}

/// The cut debounce runs on the host clock, so a revision publishes even when no
/// media ever advances a PTS (an audio-only or SI-only input). This test spends
/// real wall time on the debounce window; media timestamps stay pinned at zero
/// throughout, which is exactly the case a media-clock debounce wedges on.
#[tokio::test(start_paused = true)]
async fn debounce_opens_without_a_media_clock() {
	let sdt = |version: u8, fill: u8| make_long_section(0x42, 1, version, 0, 0, &[fill; 4]);
	let mut rig = si_rig();

	let mut input = si_packet(0x0011, &sdt(0, 0xaa));
	input.extend_from_slice(&si_packet_cc(0x0011, &sdt(0, 0xaa), 1));
	rig.import.decode(&BytesMut::from(&input[..])).unwrap();

	let name = rig.catalog.snapshot().mpegts.si[&0x0011][&0x42].track.clone();
	let track = rig.consumer.track(&name).unwrap().subscribe(None).await.unwrap();
	assert_eq!(track.latest(), Some(0), "the first snapshot cut immediately");

	// A revision inside the window is coalesced...
	rig.import
		.decode(&BytesMut::from(&si_packet_cc(0x0011, &sdt(1, 0xbb), 2)[..]))
		.unwrap();
	assert_eq!(track.latest(), Some(0), "a revision inside the window is held");

	// ...and publishes once the window passes in *real* time, no finish, no PTS.
	std::thread::sleep(std::time::Duration::from_millis(1200));
	rig.import
		.decode(&BytesMut::from(&si_packet_cc(0x0011, &sdt(1, 0xbb), 3)[..]))
		.unwrap();
	assert_eq!(track.latest(), Some(1), "the held revision cut after the window");
}
