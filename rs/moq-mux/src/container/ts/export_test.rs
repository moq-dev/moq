//! Tests for the MPEG-TS exporter.
//!
//! Audio is framed as ADTS; video is normalized to length-prefixed NALU by
//! `ExportSource` and rewritten to Annex-B by the muxer (re-injecting the
//! parameter sets on keyframes). These build a synthetic broadcast, export to
//! TS, and re-parse with the `mpeg2ts` reader.

use std::io::Cursor;

use bytes::{Bytes, BytesMut};
use hang::catalog::{AAC, AudioConfig, Container, H264, VideoConfig};
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::{ReadTsPacket, TsPacketReader, TsPayload};

use crate::catalog::hang::Container as HangContainer;
use crate::container::{Frame, Producer, Timestamp};

const SC: &[u8] = &[0, 0, 0, 1];
// Reusable H.264 parameter-set and slice NALs (NAL type = first byte & 0x1f).
const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xde];
const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

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
	let mut exporter = crate::container::ts::Export::new(consumer).unwrap();
	let mut out = BytesMut::new();
	// `while let Ok` stops on the first timeout (`Pending`: no more output).
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await {
		match res.expect("exporter error") {
			Some(chunk) => out.extend_from_slice(&chunk),
			None => break,
		}
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
	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast.unique_track(".aac").unwrap();
	let name = track.name.clone();
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
				timestamp: Timestamp::from_millis(i as u64 * 20).unwrap(),
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
	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let track = broadcast.unique_track(".avc3").unwrap();
	let name = track.name.clone();
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
			timestamp: Timestamp::from_millis(0).unwrap(),
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

/// Out-of-band avc1 (e.g. from fmp4 import): length-prefixed NALs with the
/// SPS/PPS only in the catalog `description` (avcC). The muxer must parse the
/// avcC, prepend the parameter sets as Annex-B on the keyframe, and rewrite the
/// length prefixes to start codes.
#[tokio::test(start_paused = true)]
async fn export_avc1_out_of_band_reassembles() {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let avcc = crate::codec::h264::build_avcc(SPS, PPS).unwrap();

	let track = broadcast.unique_track(".avc1").unwrap();
	let name = track.name.clone();
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
			timestamp: Timestamp::from_millis(0).unwrap(),
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
