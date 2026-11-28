//! `--program all` TS ingest across program count and per-program bitrate, over one second of
//! a generated multiplex. Throughput is the whole input, so a parse that repeats per program
//! shows as a falling rate with program count. A PAT that also names programs whose PMT lists
//! no stream shows what a program costs before it starts. Stray PIDs no table lists sweep the
//! unclaimed-PID accounting, against the same packets sent as null stuffing, which skips it.
//!
//! Run with `cargo bench -p moq-mux --bench ts_programs`.

use std::fmt;

use swole::bench;

const FPS: u64 = 25;
const GOP: u64 = 25;
/// PSI repeats every this many frames: 160 ms.
const PSI_EVERY: u64 = 4;
const SPS: &[u8] = &[
	0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00, 0x01,
	0xd4, 0xc0, 0x80,
];
const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

/// ISO/IEC 13818-1 CRC-32: polynomial 0x04C11DB7, no reflection, no final XOR.
fn crc32(data: &[u8]) -> u32 {
	let mut crc = 0xffff_ffffu32;
	for &byte in data {
		crc ^= u32::from(byte) << 24;
		for _ in 0..8 {
			crc = if crc & 0x8000_0000 != 0 {
				(crc << 1) ^ 0x04c1_1db7
			} else {
				crc << 1
			};
		}
	}
	crc
}

/// A long-form PSI section: `table_id`, the 16-bit extension, version 0, section `number` of
/// `last`.
fn section(table_id: u8, extension: u16, number: u8, last: u8, body: &[u8]) -> Vec<u8> {
	let length = 5 + body.len() + 4;
	let mut section = vec![table_id, 0xb0 | (length >> 8) as u8, length as u8];
	section.extend_from_slice(&extension.to_be_bytes());
	section.extend_from_slice(&[0xc1, number, last]);
	section.extend_from_slice(body);
	section.extend_from_slice(&crc32(&section).to_be_bytes());
	section
}

fn pmt_pid(program: u16) -> u16 {
	0x0100 + program
}

fn video_pid(program: u16) -> u16 {
	0x0200 + program
}

/// The PMT PID a PAT names for an idle program, clear of every PID that carries data.
fn idle_pid(program: u16) -> u16 {
	0x1000 + program
}

/// A PID no table lists, clear of every other.
fn stray_pid(stray: u16) -> u16 {
	0x1800 + stray
}

const NULL_PID: u16 = 0x1fff;

#[derive(Default)]
struct Mux {
	out: Vec<u8>,
	cc: std::collections::HashMap<u16, u8>,
}

impl Mux {
	fn header(&mut self, pid: u16, start: bool, adaptation: bool) {
		let cc = self.cc.entry(pid).or_default();
		self.out.extend_from_slice(&[
			0x47,
			u8::from(start) << 6 | (pid >> 8) as u8,
			pid as u8,
			if adaptation { 0x30 } else { 0x10 } | *cc,
		]);
		*cc = (*cc + 1) & 0x0f;
	}

	fn psi(&mut self, pid: u16, section: &[u8]) {
		let mut payload = vec![0];
		payload.extend_from_slice(section);
		for (i, chunk) in payload.chunks(184).enumerate() {
			self.header(pid, i == 0, false);
			let end = self.out.len() + 184;
			self.out.extend_from_slice(chunk);
			self.out.resize(end, 0xff);
		}
	}

	/// One access unit on `pid` in `packets` packets, its first carrying `pcr` (27 MHz) and a
	/// PES header with PTS `pts` (90 kHz).
	fn frame(&mut self, pid: u16, packets: usize, pcr: u64, pts: u64, keyframe: bool) {
		let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
		pes.extend_from_slice(&[
			0x21 | ((pts >> 29) & 0x0e) as u8,
			(pts >> 22) as u8,
			0x01 | ((pts >> 14) & 0xfe) as u8,
			(pts >> 7) as u8,
			0x01 | ((pts << 1) & 0xfe) as u8,
		]);
		let nals: &[&[u8]] = match keyframe {
			true => &[SPS, PPS, &[0x65, 0x88, 0x84, 0x21]],
			false => &[&[0x41, 0x9a, 0x02, 0x04]],
		};
		for nal in nals {
			pes.extend_from_slice(&[0, 0, 0, 1]);
			pes.extend_from_slice(nal);
		}
		// The first packet gives 8 bytes to its PCR's adaptation field. Nonzero padding cannot
		// add a start code.
		pes.resize(packets * 184 - 8, 0xff);

		self.header(pid, true, true);
		let (base, ext) = (pcr / 300, pcr % 300);
		self.out.extend_from_slice(&[
			7,
			0x10 | u8::from(keyframe) << 6,
			(base >> 25) as u8,
			(base >> 17) as u8,
			(base >> 9) as u8,
			(base >> 1) as u8,
			((base & 1) << 7) as u8 | 0x7e | (ext >> 8) as u8,
			ext as u8,
		]);
		let (first, rest) = pes.split_at(176);
		self.out.extend_from_slice(first);
		for chunk in rest.chunks(184) {
			self.header(pid, false, false);
			self.out.extend_from_slice(chunk);
		}
	}
}

/// What the multiplex carries besides its described programs.
#[derive(Clone, Copy)]
struct Extra {
	/// Programs the PAT names whose PMT lists no stream, so they wait without starting.
	idle: u16,
	/// PIDs no table lists, one packet each per frame.
	strays: u16,
	/// Send the strays' packets as null stuffing instead.
	null: bool,
}

/// One second of `programs` H.264 programs at `bitrate` bits per second each, interleaved
/// frame by frame, each program carrying its PCR on its video PID, beside `extra`.
fn multiplex(programs: u16, extra: Extra, bitrate: u64) -> Vec<u8> {
	let Extra { idle, strays, null } = extra;
	let packets = (bitrate / FPS / 8).div_ceil(188) as usize;
	let mut mux = Mux::default();
	let entries: Vec<u8> = (1..=programs)
		.map(|program| (program, pmt_pid(program)))
		.chain((programs + 1..=programs + idle).map(|program| (program, idle_pid(program))))
		.flat_map(|(program, pid)| {
			let [a, b] = program.to_be_bytes();
			let [c, d] = (0xe000 | pid).to_be_bytes();
			[a, b, c, d]
		})
		.collect();
	// A section holds at most 1021 bytes, so a long PAT spans several.
	let chunks: Vec<_> = entries.chunks(250 * 4).collect();
	let last = (chunks.len() - 1) as u8;
	let pat: Vec<_> = (0..)
		.zip(&chunks)
		.map(|(number, chunk)| section(0x00, 1, number, last, chunk))
		.collect();
	let pmts: Vec<_> = (1..=programs)
		.map(|program| {
			let video = video_pid(program);
			let mut body = (0xe000 | video).to_be_bytes().to_vec();
			body.extend_from_slice(&[0xf0, 0x00, 0x1b]);
			body.extend_from_slice(&(0xe000 | video).to_be_bytes());
			body.extend_from_slice(&[0xf0, 0x00]);
			section(0x02, program, 0, 0, &body)
		})
		.collect();
	// No PCR PID, no program info, no stream.
	let idle_pmts: Vec<_> = (programs + 1..=programs + idle)
		.map(|program| section(0x02, program, 0, 0, &[0xff, 0xff, 0xf0, 0x00]))
		.collect();
	for frame in 0..FPS {
		if frame % PSI_EVERY == 0 {
			for pat in &pat {
				mux.psi(0, pat);
			}
			for (program, pmt) in (1..=programs).zip(&pmts) {
				mux.psi(pmt_pid(program), pmt);
			}
			for (program, pmt) in (programs + 1..).zip(&idle_pmts) {
				mux.psi(idle_pid(program), pmt);
			}
		}
		let pts = 90_000 + frame * 90_000 / FPS;
		for program in 1..=programs {
			mux.frame(video_pid(program), packets, pts * 300, pts, frame % GOP == 0);
		}
		for stray in 0..strays {
			let pid = if null { NULL_PID } else { stray_pid(stray) };
			mux.header(pid, false, false);
			let end = mux.out.len() + 184;
			mux.out.resize(end, 0xff);
		}
	}
	mux.out
}

type Importer = (moq_mux::container::ts::Programs, moq_net::origin::Driver);

/// A fresh importer, with the origin driver it needs kept alive but unpolled.
fn importer() -> Importer {
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let programs = moq_mux::container::ts::Programs::new(
		origin,
		moq_net::Path::from_static("bench.hang"),
		moq_mux::catalog::Config::default(),
	);
	(programs, driver)
}

/// Import all of `input` and finish, for the counters.
fn ingest(programs: u16, input: &[u8]) -> moq_mux::container::ts::stats::Snapshot {
	let (mut importer, _driver) = importer();
	importer.decode(input).unwrap();
	let finished = importer.finish().unwrap();
	assert_eq!(finished.broadcasts().len(), usize::from(programs));
	finished.stats()
}

/// An input, the packets per chunk to feed it in, and an importer for each call.
struct Ingest {
	input: Vec<u8>,
	packets: usize,
	importer: Option<Importer>,
}

fn refill(ingest: &mut Ingest) {
	ingest.importer = Some(importer());
}

/// Decode the whole input, returning the importer so its drop stays outside the measurement.
fn decode(ingest: &mut Ingest) -> Importer {
	let (mut programs, driver) = ingest.importer.take().expect("refilled before each call");
	for chunk in ingest.input.chunks(188 * ingest.packets) {
		programs.decode(chunk).unwrap();
	}
	(programs, driver)
}

/// Described programs at a bitrate beside `extra`, named `<programs>[+<idle>|-<lane><strays>]/<mbps>mbps`.
#[derive(Clone, Copy)]
struct Case {
	programs: u16,
	extra: Extra,
	mbps: u64,
}

impl Case {
	fn input(self) -> Vec<u8> {
		multiplex(self.programs, self.extra, self.mbps * 1_000_000)
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let Extra { idle, strays, null } = self.extra;
		write!(f, "{}", self.programs)?;
		if idle > 0 {
			write!(f, "+{idle}")?;
		}
		if strays > 0 {
			write!(f, "-{}{strays}", if null { "null" } else { "stray" })?;
		}
		write!(f, "/{}mbps", self.mbps)
	}
}

fn cases() -> Vec<Case> {
	let mut cases = Vec::new();
	for programs in [1u16, 4, 16] {
		for idle in [0u16, 1000] {
			for mbps in [1u64, 4] {
				let extra = Extra {
					idle,
					strays: 0,
					null: false,
				};
				cases.push(Case { programs, extra, mbps });
			}
		}
		for strays in [16u16, 256] {
			for null in [true, false] {
				let extra = Extra { idle: 0, strays, null };
				cases.push(Case {
					programs,
					extra,
					mbps: 1,
				});
			}
		}
	}
	cases
}

fn programs(case: Case) -> Ingest {
	let input = case.input();
	let stats = ingest(case.programs, &input);
	let (media, strays): (Vec<_>, Vec<_>) = stats.streams.values().partition(|s| !s.track.is_empty());
	assert!(
		media.len() == usize::from(case.programs) && media.iter().all(|s| s.units == FPS),
		"every program delivers every frame"
	);
	let counted = if case.extra.null {
		0
	} else {
		usize::from(case.extra.strays)
	};
	assert!(
		strays.len() == counted && strays.iter().all(|s| s.refused == FPS),
		"every stray packet is counted"
	);
	Ingest {
		input,
		packets: 7,
		importer: None,
	}
}

#[bench(args = cases(), setup = programs, before = refill, bytes = |case: Case| case.input().len())]
fn ts_programs(ingest: &mut Ingest) -> Importer {
	decode(ingest)
}

/// Programs, and packets per input chunk, named `<programs>/<packets>packets`.
#[derive(Clone, Copy)]
struct Stalled {
	programs: u16,
	packets: usize,
}

impl Stalled {
	/// One second of `programs` programs whose every PCR repeats the first.
	fn input(self) -> Vec<u8> {
		let mut input = multiplex(
			self.programs,
			Extra {
				idle: 0,
				strays: 0,
				null: false,
			},
			1_000_000,
		);
		for packet in input.as_chunks_mut::<188>().0 {
			if packet[3] & 0x20 != 0 && packet[5] & 0x10 != 0 {
				// Repeat the first PCR (one second), preserving every PES and its PTS.
				packet[6..12].copy_from_slice(&[0, 0, 0xaf, 0xc8, 0x7e, 0]);
			}
		}
		input
	}
}

impl fmt::Display for Stalled {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}packets", self.programs, self.packets)
	}
}

fn stalled_cases() -> Vec<Stalled> {
	[1u16, 4, 16]
		.into_iter()
		.flat_map(|programs| [1, 7, 256].map(|packets| Stalled { programs, packets }))
		.collect()
}

fn stalled(case: Stalled) -> Ingest {
	let input = case.input();
	let stats = ingest(case.programs, &input);
	assert_eq!(stats.streams.len(), usize::from(case.programs));
	assert!(stats.streams.values().all(|stream| stream.units == FPS));
	Ingest {
		input,
		packets: case.packets,
		importer: None,
	}
}

/// Packet batching and program fanout vary independently while each PCR repeats its first
/// value, so no rate is measured and each program's health clock falls back to arrival,
/// settled per input chunk, even for programs no packet routes to.
#[bench(args = stalled_cases(), setup = stalled, before = refill, bytes = |case: Stalled| case.input().len())]
fn ts_stalled(ingest: &mut Ingest) -> Importer {
	decode(ingest)
}

fn main() {
	swole::main();
}
