//! Builds transport streams packet by packet for the passthrough and release tests, covering
//! what the in-repo captures don't carry (several programs, a wrap, a flagged jump, ...).

use std::collections::HashMap;

use super::psi;

/// Builds a transport stream packet by packet.
#[derive(Default)]
pub(super) struct Mux {
	pub(super) out: Vec<u8>,
	cc: HashMap<u16, u8>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Flags {
	pub(super) pcr: Option<u64>,
	pub(super) random_access: bool,
	pub(super) discontinuity: bool,
}

impl Flags {
	pub(super) fn pcr(ticks: u64) -> Self {
		Self {
			pcr: Some(ticks),
			..Default::default()
		}
	}

	pub(super) fn random_access(mut self) -> Self {
		self.random_access = true;
		self
	}

	pub(super) fn discontinuity(mut self) -> Self {
		self.discontinuity = true;
		self
	}
}

pub(super) const ACCESS: Flags = Flags {
	pcr: None,
	random_access: true,
	discontinuity: false,
};

impl Mux {
	fn cc(&mut self, pid: u16) -> u8 {
		let cc = self.cc.entry(pid).or_default();
		let out = *cc;
		*cc = (*cc + 1) & 0x0f;
		out
	}

	/// One packet on `pid`, its payload filled with the PID's low byte.
	pub(super) fn packet(&mut self, pid: u16, flags: Flags) -> &mut Self {
		let cc = self.cc(pid);
		let mut pkt = vec![0x47, (pid >> 8) as u8 & 0x1f, pid as u8];
		let marked = flags.pcr.is_some() || flags.random_access || flags.discontinuity;
		if marked {
			pkt.push(0x30 | cc);
			let mut field = vec![
				u8::from(flags.discontinuity) << 7
					| u8::from(flags.random_access) << 6
					| u8::from(flags.pcr.is_some()) << 4,
			];
			if let Some(pcr) = flags.pcr {
				let (base, ext) = (pcr / 300, pcr % 300);
				field.extend_from_slice(&[
					(base >> 25) as u8,
					(base >> 17) as u8,
					(base >> 9) as u8,
					(base >> 1) as u8,
					((base & 1) << 7) as u8 | 0x7e | (ext >> 8) as u8,
					ext as u8,
				]);
			}
			pkt.push(field.len() as u8);
			pkt.extend_from_slice(&field);
		} else {
			pkt.push(0x10 | cc);
		}
		pkt.resize(188, pid as u8);
		self.out.extend_from_slice(&pkt);
		self
	}

	/// `n` plain packets on `pid`.
	pub(super) fn fill(&mut self, pid: u16, n: usize) -> &mut Self {
		for _ in 0..n {
			self.packet(pid, Flags::default());
		}
		self
	}

	/// A packet on `pid` that is all adaptation field: no flags, then stuffing to the end.
	pub(super) fn stuffing(&mut self, pid: u16) -> &mut Self {
		// An adaptation-only packet repeats the counter of the packet before it.
		let cc = self.cc.get(&pid).map_or(0, |next| next.wrapping_sub(1) & 0x0f);
		let mut pkt = vec![0x47, (pid >> 8) as u8 & 0x1f, pid as u8, 0x20 | cc, 183, 0x00];
		pkt.resize(188, 0xff);
		self.out.extend_from_slice(&pkt);
		self
	}

	/// The first packet of a PES on `pid`, carrying `flags` and a header with a PTS of `pts`
	/// (90 kHz); the PES continues in whatever packets on `pid` follow.
	pub(super) fn pes(&mut self, pid: u16, flags: Flags, pts: u64) -> &mut Self {
		let at = self.out.len();
		self.packet(pid, flags);
		let pkt = &mut self.out[at..];
		pkt[1] |= 0x40;
		let payload = match pkt[3] & 0x20 != 0 {
			true => 5 + usize::from(pkt[4]),
			false => 4,
		};
		let pts = [
			0x21 | ((pts >> 29) & 0x0e) as u8,
			(pts >> 22) as u8,
			0x01 | ((pts >> 14) & 0xfe) as u8,
			(pts >> 7) as u8,
			0x01 | ((pts << 1) & 0xfe) as u8,
		];
		let stream_id = if pid == 0x100 { 0xe0 } else { 0xc0 };
		let header = [0, 0, 1, stream_id, 0, 0, 0x80, 0x80, 5];
		pkt[payload..payload + header.len()].copy_from_slice(&header);
		pkt[payload + header.len()..payload + header.len() + pts.len()].copy_from_slice(&pts);
		self
	}

	/// A whole PES on `pid`, split into as many packets as it needs, the last padded with
	/// adaptation stuffing so the PES ends with it. Returns where its start code sits in the
	/// first packet, and the packets, numbered on `pid` in order.
	pub(super) fn split(&mut self, pid: u16, pes: &[u8]) -> (usize, Vec<Vec<u8>>) {
		let mut start = 4;
		let mut packets = Vec::new();
		for (i, chunk) in pes.chunks(184).enumerate() {
			let cc = self.cc(pid);
			let pusi = if i == 0 { 0x40 } else { 0 };
			let mut pkt = vec![0x47, pusi | (pid >> 8) as u8 & 0x1f, pid as u8];
			if chunk.len() == 184 {
				pkt.push(0x10 | cc);
			} else {
				let field = 183 - chunk.len();
				pkt.extend_from_slice(&[0x30 | cc, field as u8]);
				if field > 0 {
					pkt.push(0x00);
					pkt.resize(5 + field, 0xff);
				}
			}
			if i == 0 {
				start = pkt.len();
			}
			pkt.extend_from_slice(chunk);
			packets.push(pkt);
		}
		(start, packets)
	}

	/// A whole PES on `pid`, as [`Self::split`] packetizes it. Returns the input offset of its
	/// start code.
	pub(super) fn whole(&mut self, pid: u16, pes: &[u8]) -> u64 {
		let (start, packets) = self.split(pid, pes);
		let at = (self.out.len() + start) as u64;
		self.out.extend(packets.concat());
		at
	}

	/// A section on `pid`, split across as many packets as it needs.
	pub(super) fn section(&mut self, pid: u16, section: &[u8]) -> &mut Self {
		let mut payload = vec![0u8];
		payload.extend_from_slice(section);
		for (i, chunk) in payload.chunks(184).enumerate() {
			let cc = self.cc(pid);
			let pusi = if i == 0 { 0x40 } else { 0 };
			let mut pkt = vec![0x47, pusi | (pid >> 8) as u8 & 0x1f, pid as u8, 0x10 | cc];
			pkt.extend_from_slice(chunk);
			pkt.resize(188, 0xff);
			self.out.extend_from_slice(&pkt);
		}
		self
	}

	pub(super) fn pat(&mut self, programs: &[(u16, u16)]) -> &mut Self {
		self.pat_in(1, programs)
	}

	/// A [`Self::pat`] naming `transport_stream_id`.
	pub(super) fn pat_in(&mut self, transport_stream_id: u16, programs: &[(u16, u16)]) -> &mut Self {
		let body: Vec<u8> = programs
			.iter()
			.flat_map(|&(number, pmt)| [(number >> 8) as u8, number as u8, 0xe0 | (pmt >> 8) as u8, pmt as u8])
			.collect();
		self.section(0, &psi::section(psi::PAT_TABLE_ID, transport_stream_id, 0, 0, 0, &body))
	}

	/// A PMT whose `program_info` holds `info` bytes of private descriptors, so a large one
	/// spans packets.
	pub(super) fn pmt(
		&mut self,
		pid: u16,
		program: u16,
		pcr_pid: u16,
		streams: &[(u8, u16)],
		info: usize,
	) -> &mut Self {
		let streams: Vec<_> = streams
			.iter()
			.map(|&(stream_type, es)| (stream_type, es, &[][..]))
			.collect();
		self.pmt_with(pid, program, pcr_pid, &streams, info)
	}

	/// A [`Self::pmt`] whose streams each carry `(stream_type, pid, ES descriptors)`.
	pub(super) fn pmt_with(
		&mut self,
		pid: u16,
		program: u16,
		pcr_pid: u16,
		streams: &[(u8, u16, &[u8])],
		info: usize,
	) -> &mut Self {
		self.pmt_at(0, pid, program, pcr_pid, streams, info)
	}

	/// A [`Self::pmt_with`] at `version_number` `version`.
	pub(super) fn pmt_at(
		&mut self,
		version: u8,
		pid: u16,
		program: u16,
		pcr_pid: u16,
		streams: &[(u8, u16, &[u8])],
		info: usize,
	) -> &mut Self {
		let mut descriptors = Vec::new();
		while descriptors.len() < info {
			let len = (info - descriptors.len()).clamp(2, 200) - 2;
			descriptors.push(0x80);
			descriptors.push(len as u8);
			descriptors.extend(std::iter::repeat_n(0xaa, len));
		}
		let mut body = vec![
			0xe0 | (pcr_pid >> 8) as u8,
			pcr_pid as u8,
			0xf0 | (descriptors.len() >> 8) as u8,
			descriptors.len() as u8,
		];
		body.extend_from_slice(&descriptors);
		for &(stream_type, es, info) in streams {
			body.extend_from_slice(&[
				stream_type,
				0xe0 | (es >> 8) as u8,
				es as u8,
				0xf0 | (info.len() >> 8) as u8,
				info.len() as u8,
			]);
			body.extend_from_slice(info);
		}
		self.section(pid, &psi::section(psi::PMT_TABLE_ID, program, version, 0, 0, &body))
	}

	pub(super) fn bytes(&self) -> Vec<u8> {
		self.out.clone()
	}
}

/// 27 MHz ticks per millisecond.
pub(super) const MS: u64 = 27_000;

/// A single program: PMT on 0x20, H.264 video on 0x100, audio on 0x101, the clock on `pcr_pid`.
pub(super) fn program(mux: &mut Mux, pcr_pid: u16) {
	mux.pat(&[(1, 0x20)])
		.pmt(0x20, 1, pcr_pid, &[(0x1b, 0x100), (0x0f, 0x101)], 0);
}

/// `seconds` of a stream paced 40 ms per PCR on `pcr_pid`, starting at `start` ticks, with a
/// video random access point every `gop` PCRs (none when zero). Each PCR interval is the PCR
/// packet then `fill` packets alternating video and audio.
pub(super) fn paced(mux: &mut Mux, pcr_pid: u16, start: u64, intervals: u64, gop: u64, fill: usize) {
	for i in 0..intervals {
		mux.packet(pcr_pid, Flags::pcr(start + i * 40 * MS));
		if gop != 0 && i % gop == 0 {
			mux.packet(0x100, ACCESS);
		}
		for j in 0..fill {
			mux.fill(if j % 2 == 0 { 0x100 } else { 0x101 }, 1);
		}
	}
}

/// 2^33 PCR bases, where the clock wraps, in 27 MHz ticks.
pub(super) const WRAP: u64 = (1 << 33) * 300;

/// One program carrying everything a raw passthrough has to leave untouched, `intervals` PCRs
/// 40 ms apart from `start` ticks, wrapping at [`WRAP`]: the clock alone on PID 0x1ff, PSI and
/// an SDT before each random access point, a video PES started mid-interval so it is split
/// across the next PCR, audio PES, adaptation stuffing, null packets, and a PID no table lists.
pub(super) fn assorted(start: u64, intervals: u64, gop: u64) -> Vec<u8> {
	let mut mux = Mux::default();
	let sdt = psi::section(0x42, 1, 0, 0, 0, &[0x00, 0x01, 0xff, 0x00, 0x01, 0xfc, 0x80, 0x00]);
	for i in 0..intervals {
		let ticks = (start + i * 40 * MS) % WRAP;
		if i % gop == 0 {
			mux.pat(&[(1, 0x20)])
				.pmt(0x20, 1, 0x1ff, &[(0x1b, 0x100), (0x0f, 0x101)], 0)
				.section(0x11, &sdt);
		}
		mux.packet(0x1ff, Flags::pcr(ticks));
		mux.fill(0x100, 2);
		let pts = (ticks / 300 + 90 * 100) % (1 << 33);
		match i % gop {
			0 => mux.pes(0x100, ACCESS, pts),
			_ => mux.pes(0x100, Flags::default(), pts),
		};
		mux.fill(0x100, 2).pes(0x101, Flags::default(), pts).stuffing(0x101);
		mux.fill(0x1fff, 2);
		if i % 5 == 0 {
			mux.fill(0x1abc, 1);
		}
	}
	mux.bytes()
}

/// Two programs on clocks of their own, `intervals` of program 1's PCRs 40 ms apart, each with
/// a random access point every `gop` intervals but never at the same time as the other's.
pub(super) fn mpts(intervals: u64, gop: u64) -> Vec<u8> {
	let mut mux = Mux::default();
	// Program 2's clock is unrelated to program 1's and ticks on its own cadence.
	let other = WRAP / 3;
	for i in 0..intervals {
		if i % gop == 0 {
			mux.pat(&[(1, 0x20), (2, 0x30)])
				.pmt(0x20, 1, 0x100, &[(0x1b, 0x100), (0x0f, 0x101)], 0)
				.pmt(0x30, 2, 0x200, &[(0x1b, 0x200), (0x0f, 0x201)], 0);
		}
		let mut flags = Flags::pcr(i * 40 * MS);
		flags.random_access = i % gop == 0;
		mux.pes(0x100, flags, i * 3_600);
		mux.fill(0x100, 3).fill(0x101, 1);
		if i % 3 == 1 {
			mux.packet(0x200, Flags::pcr(other + i * 40 * MS + 17 * MS));
		}
		if i % gop == gop / 2 + 1 {
			mux.pes(0x200, ACCESS, i * 3_600);
		}
		mux.fill(0x200, 3).fill(0x201, 1).fill(0x1fff, 1);
	}
	mux.bytes()
}
