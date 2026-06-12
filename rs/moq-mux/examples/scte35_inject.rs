//! One-shot SCTE-35 fixture generator: inject splice sections into an MPEG-TS.
//!
//! Reads an input TS (e.g. an ffmpeg/GStreamer clip), re-declares its program with a
//! SCTE-35 PID (program-level CUEI + stream_type 0x86), and splices five
//! `splice_info_section`s at 5/25/50/70/85% of the packet span, spread far enough apart
//! to land on distinct video frames in these clips.
//! Writes the result. Used to build the checked-in SCTE-35 test fixtures.
//!
//!   cargo run -p moq-mux --example scte35_inject -- input.ts output.ts <set>
//!
//! `<set>` picks the cue set (one per fixture family, see [`SETS`]).

use std::io::Cursor;

use base64::Engine;
use mpeg2ts::es::StreamType;
use mpeg2ts::ts::payload::Pmt;
use mpeg2ts::ts::{
	ContinuityCounter, Descriptor, EsInfo, Pid, ReadTsPacket, TransportScramblingControl, TsHeader, TsPacket,
	TsPacketReader, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
};

const SCTE_PID: u16 = 0x0021;

// One cue set per fixture: 4 real splice_info_sections from the threefive project
// (MIT, github.com/futzu/threefive) plus 1 custom built for these fixtures and
// validated with the threefive decoder. The 15 sections are all distinct.
const SETS: [(&str, [&str; 5]); 3] = [
	// splice_insert family: canonical w/ break_duration + avail descriptor; OUT w/o
	// duration; IN w/ duration; w/ DTMF descriptor; custom (event 0xED150001, 30 s break).
	(
		"gst480i",
		[
			"/DAvAAAAAAAA///wFAVIAACPf+/+c2nALv4AUsz1AAAAAAAKAAhDVUVJAAABNWLbowo=",
			"/DAqAAAAAAAA///wDwUAAAASf0/+dihKegABEv8ACgAIQ1VFSQAAABIe1kvb",
			"/DAvAAAAAAAA///wFAVAAAT2f+/+eMpEWX4A9zFAAAEL/wAKAAhDVUVJAAAACwRZmfY=",
			"/DAsAAAAAAAAAP/wDwUAAABef0/+zPACTQAAAAAADAEKQ1VFSbGfMTIxIxGolm0=",
			"/DAlAAAAAAAAAP/wFAXtFQABf+/+AKupUP4AKTLg7RUBAQAAHeTNOw==",
		],
	),
	// time_signal/segmentation family: placement opportunity; URN-UUID UPID; three
	// segmentation descriptors; multi-UPID w/ pts_time > 2^32; custom (avail "EDIS",
	// pts_time > 2^32).
	(
		"bbb5s",
		[
			"/DAvAAAAAAAA///wBQb+dGKQoAAZAhdDVUVJSAAAjn+fCAgAAAAALKChijUCAKnMZ1g=",
			"/DBZAAAAAAAA///wBQb+AAAAAABDAkFDVUVJAAAACn//AAApMuAPLXVybjp1dWlkOmFhODViYmI2LTVjNDMtNGI2YS1iZWJiLWVlM2IxM2ViNzk5ORAAAFz7UQA=",
			"/DBhAAAAAAAA///wBQb+qM1E7QBLAhdDVUVJSAAArX+fCAgAAAAALLLXnTUCAAIXQ1VFSUgAACZ/nwgIAAAAACyy150RAAACF0NVRUlIAAAnf58ICAAAAAAsstezEAAAihiGnw==",
			"/DCSAAAAAAAAAP/wBQb/RgeVUgB8AhdDVUVJbs6+VX+/CAgAAAAABy0IxzELGQIXQ1VFSW7MmIh/vwgIAAABGDayFhE3AQECHENVRUluzw0If/8AABvLoAgIAAAAAActVhIwDBkCKkNVRUluzw02f78MG1JUTE4xSAEAAAAAMTM3NjkyMDI1NDQ5NUgxAAEAAGnbuXg=",
			"/DAgAAAAAAAAAP/wBQb/I0VniQAKAAhDVUVJRURJU2sX/aw=",
		],
	),
	// misc: bare splice_null; bare time_signal; time_signal w/ private descriptor;
	// time_signal w/ avail descriptor; custom (splice_null + DTMF "159#").
	(
		"ffmpeg",
		[
			"/DARAAAAAAAAAP/wAAAAAHpPv/8=",
			"/DAWAAAAAAAAAP/wBQb+e2KfxwAAN6nTrw==",
			"/DAvAAAAAAAAAP/wBQb+Bp9rxgAZLxdmdWZ1dGhyZWVmaXZlIGtpY2tzIGFzc1m+EsU=",
			"/DAgAAAAAAAAAP/wBQb+Qjo1vQAKAAhDVUVJAAAE0iVuWvA=",
			"/DAdAAAAAAAAAP/wAAAADAEKQ1VFSVCfMTU5I+Fj87s=",
		],
	),
];

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let input = std::fs::read(&args[1]).expect("read input TS");
	let out_path = &args[2];
	let set = args.get(3).expect("usage: scte35_inject input.ts output.ts <set>");
	let cues_b64 = &SETS
		.iter()
		.find(|(name, _)| name == set)
		.unwrap_or_else(|| panic!("unknown set '{set}'; available: gst480i, bbb5s, ffmpeg"))
		.1;

	// Learn the PMT PID and the original program tables.
	let mut pmt_pid = None;
	let mut orig = None;
	let mut reader = TsPacketReader::new(Cursor::new(&input));
	while let Some(pkt) = reader.read_ts_packet().unwrap() {
		match pkt.payload {
			Some(TsPayload::Pat(pat)) => pmt_pid = pat.table.first().map(|p| p.program_map_pid),
			Some(TsPayload::Pmt(pmt)) => {
				orig = Some(pmt);
				break;
			}
			_ => {}
		}
	}
	let pmt_pid = pmt_pid.expect("input has no PAT/PMT PID");
	let orig = orig.expect("input has no PMT");

	// Augmented PMT: keep every original ES (so the reader keeps routing video/audio) and
	// add the SCTE-35 PID plus the program-level CUEI registration descriptor.
	let program_num = orig.program_num;
	let pcr_pid = orig.pcr_pid;
	let mut es_info = orig.es_info;
	let mut program_info = orig.program_info;
	es_info.push(EsInfo {
		stream_type: StreamType::Dts8ChannelLosslessAudio,
		elementary_pid: Pid::new(SCTE_PID).unwrap(),
		descriptors: vec![],
	});
	program_info.push(Descriptor {
		tag: 0x05,
		data: b"CUEI".to_vec(),
	});
	let pmt = Pmt {
		program_num,
		pcr_pid,
		version_number: VersionNumber::default(),
		program_info,
		es_info,
	};
	let mut aug_pmt = Vec::new();
	let packet = TsPacket {
		header: TsHeader {
			transport_error_indicator: false,
			transport_priority: false,
			pid: pmt_pid,
			transport_scrambling_control: TransportScramblingControl::NotScrambled,
			continuity_counter: ContinuityCounter::default(),
		},
		adaptation_field: None,
		payload: Some(TsPayload::Pmt(pmt)),
	};
	TsPacketWriter::new(&mut aug_pmt).write_ts_packet(&packet).unwrap();

	// Decode the cue sections and wrap each in a TS packet.
	let cues: Vec<Vec<u8>> = cues_b64
		.iter()
		.enumerate()
		.map(|(i, b64)| {
			let section = base64::engine::general_purpose::STANDARD
				.decode(b64)
				.expect("valid base64 cue");
			cue_packet(i as u8, &section)
		})
		.collect();

	// Pass the input packets through; insert the augmented PMT right after the first PMT,
	// and splice the cues in at evenly spread positions so each lands on a later PTS.
	let packets: Vec<&[u8]> = input.chunks(188).collect();
	let pmt_idx = packets
		.iter()
		.position(|p| pid_of(p) == pmt_pid.as_u16())
		.expect("input has no PMT packet");
	let span = packets.len() - pmt_idx;
	let cue_at: Vec<usize> = [5, 25, 50, 70, 85]
		.iter()
		.map(|pct| pmt_idx + pct * span / 100)
		.collect();

	let mut out = Vec::new();
	for (i, p) in packets.iter().enumerate() {
		out.extend_from_slice(p);
		if i == pmt_idx {
			out.extend_from_slice(&aug_pmt);
		}
		if let Some(c) = cue_at.iter().position(|&pos| pos == i) {
			out.extend_from_slice(&cues[c]);
		}
	}
	std::fs::write(out_path, &out).expect("write output TS");
	eprintln!(
		"wrote {out_path}: {} packets, {} cues from set '{set}' (pmt_pid={pmt_pid:?})",
		out.len() / 188,
		cues.len()
	);
}

fn pid_of(pkt: &[u8]) -> u16 {
	(((pkt[1] & 0x1f) as u16) << 8) | pkt[2] as u16
}

fn cue_packet(cc: u8, section: &[u8]) -> Vec<u8> {
	let mut p = vec![
		0x47,
		0x40 | ((SCTE_PID >> 8) as u8 & 0x1f),
		(SCTE_PID & 0xff) as u8,
		0x10 | (cc & 0x0f),
		0x00, // pointer_field
	];
	p.extend_from_slice(section);
	assert!(p.len() <= 188, "section too large for a single TS packet");
	p.resize(188, 0xff);
	p
}
