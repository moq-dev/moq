//! The packet layer of a TS import, run once however many programs read the input.

use std::collections::HashMap;

use bytes::Bytes;
use mpeg2ts::ts::{Pid, TsPacket};

use super::catalog::SI_PIDS;
use super::health::{Continuation, Errors, Multiplex};
use super::import::{Framer, PatReader, SectionReassembler, adaptation_valid, pcr};
use super::passthrough::{PCR_OFFSET, Sample};
use super::psi;

const NULL_PID: u16 = 0x1fff;

/// Frames the input into packets and reads what every program shares: sync, each PID's
/// continuity, the latest PCR on each PID, the PAT, and the standalone SI sections.
#[derive(Default)]
pub(super) struct Demux {
	/// Whole-packet accumulator; a trailing partial packet waits for the next call.
	scratch: Vec<u8>,
	/// The first byte of `scratch` not yet framed.
	off: usize,
	framer: Framer,
	/// The input offset of the first byte in `scratch`.
	position: u64,
	/// Packets framed so far, every PID and null stuffing included.
	packets: u64,
	health: Multiplex,
	pat: PatReader,
	/// PAT sections dropped for a bad CRC.
	pat_crc_error: u64,
	/// The latest PCR on each PID, so a program whose PMT names its clock starts from the PCR
	/// already seen. The 13-bit PID namespace bounds it.
	pcrs: HashMap<u16, Sample>,
	/// Reassemblers for the standalone SI PIDs, when anything reads them.
	si: Option<HashMap<u16, SectionReassembler>>,
	/// The SI sections the current packet completed.
	sections: Vec<Bytes>,
}

/// One packet, and what the shared layer read out of it.
pub(super) struct Packet<'a> {
	pub pkt: &'a [u8; TsPacket::SIZE],
	pub pid: u16,
	/// The packet's offset in the input.
	pub offset: u64,
	/// Packets framed through this one, every PID and null stuffing included.
	pub count: u64,
	/// What it means for the bytes already accumulated on its PID.
	pub continuation: Continuation,
	/// The whole PAT this packet completed.
	pub pat: Option<psi::Pat>,
	/// The SI sections this packet completed.
	pub sections: &'a [Bytes],
	/// The latest PCR on each PID, through this packet.
	pub pcrs: &'a HashMap<u16, Sample>,
}

impl Demux {
	/// A parser that reassembles the standalone SI PIDs when `si` is set.
	pub fn new(si: bool) -> Self {
		Self {
			si: si.then(HashMap::new),
			..Default::default()
		}
	}

	/// Append input; [`next`](Self::next) frames it.
	pub fn push(&mut self, data: &[u8]) {
		self.scratch.extend_from_slice(data);
	}

	/// The next whole packet pushed, or `None` once only a partial packet remains, at which
	/// point the caller [`drain`](Self::drain)s.
	pub fn next(&mut self) -> Option<Packet<'_>> {
		let at = self.framer.next(&self.scratch, &mut self.off)?;
		self.health.routed(&self.scratch, at);
		self.packets += 1;
		let offset = self.position + at as u64;
		let pkt: &[u8; TsPacket::SIZE] = self.scratch[at..at + TsPacket::SIZE].try_into().unwrap();
		let pid = (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]);
		let continuation = self.health.packet(pkt);

		// A retransmission repeats a clock sample rather than taking a new one, and a packet the
		// demodulator flagged corrupt has an adaptation field as untrustworthy as its payload.
		if continuation != Continuation::Duplicate
			&& pid != NULL_PID
			&& pkt[1] & 0x80 == 0
			&& adaptation_valid(pkt)
			&& let Some(ticks) = pcr(pkt)
		{
			self.pcrs.insert(
				pid,
				Sample {
					at: offset + PCR_OFFSET,
					ticks,
				},
			);
		}

		let mut pat = None;
		self.sections.clear();
		if pid == Pid::PAT {
			pat = self.pat.push(pkt, &mut self.pat_crc_error);
		} else if let Some(si) = &mut self.si
			&& SI_PIDS.contains(&pid)
		{
			let mut sections = Vec::new();
			si.entry(pid).or_default().push(pkt, &mut sections);
			self.sections.extend(sections.into_iter().map(Bytes::from));
		}

		Some(Packet {
			pkt,
			pid,
			offset,
			count: self.packets,
			continuation,
			pat,
			sections: &self.sections,
			pcrs: &self.pcrs,
		})
	}

	/// Where [`next`](Self::next) frames from: the input offset of the first byte not yet framed,
	/// and the framer's state there, for [`peek`](Self::peek) to read ahead from.
	pub fn cursor(&self) -> (u64, Framer) {
		(self.position + self.off as u64, self.framer)
	}

	/// Frame the next whole packet at or after input offset `*at` without routing it, advancing
	/// `*at` and `framer` as [`next`](Self::next) would. `None` once only a partial packet
	/// remains, with both left where more input resumes.
	pub fn peek(&self, at: &mut u64, framer: &mut Framer) -> Option<(u64, &[u8; TsPacket::SIZE])> {
		let mut off = usize::try_from(*at - self.position).expect("an offset in the pushed input");
		let found = framer.next(&self.scratch, &mut off);
		*at = self.position + off as u64;
		let found = found?;
		let pkt = self.scratch[found..found + TsPacket::SIZE].try_into().unwrap();
		Some((self.position + found as u64, pkt))
	}

	/// The input pushed but not yet framed: what [`next`](Self::next) would read on.
	pub fn remainder(&self) -> Bytes {
		Bytes::copy_from_slice(&self.scratch[self.off..])
	}

	/// Drop the framed input, keeping a trailing partial packet for the next push.
	pub fn drain(&mut self) {
		self.health.drained(&self.scratch, self.off);
		self.scratch.drain(..self.off);
		self.position += self.off as u64;
		self.off = 0;
	}

	/// No input follows: release the storage that held a partial packet for the next push.
	pub fn finish(&mut self) {
		self.scratch = Vec::new();
	}

	/// `pid` changed route: its next packet starts a fresh chain.
	pub fn restart(&mut self, pid: u16) {
		self.health.restart(pid);
	}

	/// The TR 101 290 counts read off packets alone.
	pub fn errors(&self) -> &Errors {
		self.health.errors()
	}

	/// PAT sections dropped for a bad CRC.
	pub fn pat_crc_error(&self) -> u64 {
		self.pat_crc_error
	}

	/// Input held for the next push: at most a partial packet once drained.
	#[cfg(test)]
	pub fn held(&self) -> usize {
		self.scratch.len()
	}
}
