//! Which PIDs a multiplex's tables claim, and the packets on every other one.
//!
//! A PID is claimed by the PAT (PID 0 and each listed program's PMT PID), by a delivered PMT
//! (its elementary streams and its PCR PID), or by a fixed table the importer captures (the SI
//! PIDs). The null PID is stuffing and never counted. A packet on any other PID is dropped, but
//! only once every program the PAT lists has delivered its PMT: until then a PID may belong to
//! a program whose PMT is still on its way, as on a mid-stream join. A PMT that lapses past
//! TR 101 290's 0.5 s stops holding that back.

use std::collections::{BTreeMap, HashMap, HashSet};

use mpeg2ts::ts::Pid;

/// TS packets that carry nothing: stuffing, rebuilt on export.
const NULL_PID: u16 = 0x1fff;

/// `PMT_error_2`: a PMT recurs at least every 0.5 s of program clock.
const PMT_LIMIT: u64 = super::passthrough::PCR_HZ / 2;

/// The claims of every program a PAT lists, and the packets on the PIDs none of them claim.
pub(super) struct Claims {
	/// PIDs claimed whatever the tables say beside the PAT's: the SI the importer captures.
	fixed: &'static [u16],
	/// Every PID a listed program claims, with how many claims name it.
	claimed: HashMap<u16, u32>,
	/// Each program the PAT lists, by program number. Empty until a PAT lists one.
	programs: HashMap<u16, Program>,
	/// Packets dropped on each PID, cumulative across its claims.
	dropped: BTreeMap<u16, u64>,
	/// The PIDs dropped since a table last claimed them, whose row is the dropped one.
	unclaimed: HashSet<u16>,
}

/// One listed program's claims.
struct Program {
	pmt_pid: u16,
	/// The elementary stream and PCR PIDs of its last PMT, `None` until one arrives.
	pids: Option<Vec<u16>>,
	/// The program clock when the PAT listed it, `None` until the clock starts.
	listed: Option<u64>,
	/// Its PMT lapsed before arriving, so it no longer holds the count back.
	lapsed: bool,
}

impl Claims {
	/// Claims for an importer that captures the SI on `si`, if any.
	pub(super) fn new(si: &'static [u16]) -> Self {
		Self {
			fixed: si,
			claimed: HashMap::new(),
			programs: HashMap::new(),
			dropped: BTreeMap::new(),
			unclaimed: HashSet::new(),
		}
	}

	/// A whole PAT listing `programs` as `(program_number, pmt_pid)`, the network entry excluded,
	/// at program clock `now`. A program it no longer lists releases its claims; a new one, or
	/// one moved to another PMT PID, holds the count back until its PMT arrives or lapses.
	pub(super) fn pat(&mut self, programs: &[(u16, Pid)], now: Option<u64>) {
		let stale: Vec<u16> = self
			.programs
			.iter()
			.filter(|(number, program)| {
				!programs
					.iter()
					.any(|(listed, pid)| listed == *number && pid.as_u16() == program.pmt_pid)
			})
			.map(|(&number, _)| number)
			.collect();
		for number in stale {
			let program = self.programs.remove(&number).expect("a listed program");
			self.release(program.pmt_pid);
			for pid in program.pids.into_iter().flatten() {
				self.release(pid);
			}
		}
		for &(number, pmt_pid) in programs {
			if self.programs.contains_key(&number) {
				continue;
			}
			self.claim(pmt_pid.as_u16());
			self.programs.insert(
				number,
				Program {
					pmt_pid: pmt_pid.as_u16(),
					pids: None,
					listed: now,
					lapsed: false,
				},
			);
		}
	}

	/// The PMT of `program` lists `pids`, its elementary streams and PCR PID. Ignored for a
	/// program the PAT does not list.
	pub(super) fn pmt(&mut self, program: u16, pids: impl IntoIterator<Item = u16>) {
		let Some(entry) = self.programs.get_mut(&program) else {
			return;
		};
		let mut pids: Vec<u16> = pids.into_iter().filter(|&pid| pid != NULL_PID).collect();
		pids.sort_unstable();
		pids.dedup();
		let old = entry.pids.replace(pids.clone()).unwrap_or_default();
		for pid in pids {
			self.claim(pid);
		}
		for pid in old {
			self.release(pid);
		}
	}

	/// The program clock reached `now`: a PMT awaited for longer than 0.5 s lapses.
	pub(super) fn tick(&mut self, now: u64) {
		for program in self.programs.values_mut() {
			if program.pids.is_some() || program.lapsed {
				continue;
			}
			let listed = *program.listed.get_or_insert(now);
			program.lapsed = now.saturating_sub(listed) > PMT_LIMIT;
		}
	}

	/// Count a packet on `pid` if no table claims it and every listed PMT is in.
	pub(super) fn packet(&mut self, pid: u16) {
		if pid == NULL_PID
			|| pid == Pid::PAT
			|| self.claimed.contains_key(&pid)
			|| self.fixed.contains(&pid)
			|| !self.acquired()
		{
			return;
		}
		*self.dropped.entry(pid).or_default() += 1;
		if self.unclaimed.insert(pid) {
			tracing::warn!(
				pid,
				"dropping a PID no PAT, PMT or captured SI claims; carry the multiplex whole to keep it"
			);
		}
	}

	/// Whether a PAT listed a program and every one it lists has delivered or lapsed its PMT.
	fn acquired(&self) -> bool {
		!self.programs.is_empty()
			&& self
				.programs
				.values()
				.all(|program| program.pids.is_some() || program.lapsed)
	}

	/// Each PID with dropped packets: how many, and whether it is dropped now rather than
	/// claimed again since.
	pub(super) fn rows(&self) -> impl Iterator<Item = (u16, u64, bool)> + '_ {
		self.dropped
			.iter()
			.map(|(&pid, &dropped)| (pid, dropped, self.unclaimed.contains(&pid)))
	}

	fn claim(&mut self, pid: u16) {
		*self.claimed.entry(pid).or_default() += 1;
		self.unclaimed.remove(&pid);
	}

	fn release(&mut self, pid: u16) {
		if let std::collections::hash_map::Entry::Occupied(mut entry) = self.claimed.entry(pid) {
			*entry.get_mut() -= 1;
			if *entry.get() == 0 {
				entry.remove();
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn pid(pid: u16) -> Pid {
		Pid::new(pid).unwrap()
	}

	/// Nothing counts before every listed PMT is in, and a claimed, fixed or null PID never.
	#[test]
	fn counts_only_unclaimed_pids_once_acquired() {
		let mut claims = Claims::new(&[0x10]);
		claims.packet(0x300);
		claims.pat(&[(1, pid(0x20)), (2, pid(0x30))], Some(0));
		claims.pmt(1, [0x100, 0x101]);
		claims.packet(0x300);
		assert_eq!(claims.rows().count(), 0, "program 2's PMT is still awaited");
		claims.pmt(2, [0x200]);
		for pid in [0x0000, 0x0010, 0x0020, 0x0030, 0x0100, 0x0200, NULL_PID, 0x0300, 0x0300] {
			claims.packet(pid);
		}
		assert_eq!(claims.rows().collect::<Vec<_>>(), [(0x300, 2, true)]);
	}

	/// A PMT that never arrives stops holding the count back 0.5 s after the PAT listed it.
	#[test]
	fn a_lapsed_pmt_releases_the_count() {
		let mut claims = Claims::new(&[]);
		claims.pat(&[(1, pid(0x20)), (2, pid(0x30))], Some(0));
		claims.pmt(1, [0x100]);
		claims.tick(PMT_LIMIT);
		claims.packet(0x300);
		assert_eq!(claims.rows().count(), 0);
		claims.tick(PMT_LIMIT + 1);
		claims.packet(0x300);
		assert_eq!(claims.rows().collect::<Vec<_>>(), [(0x300, 1, true)]);
	}

	/// A PID two programs claim stays claimed until both release it.
	#[test]
	fn a_shared_pid_is_released_by_its_last_claim() {
		let mut claims = Claims::new(&[]);
		claims.pat(&[(1, pid(0x20)), (2, pid(0x30))], Some(0));
		claims.pmt(1, [0x100, 0x1ff]);
		claims.pmt(2, [0x200, 0x1ff]);
		claims.pmt(1, [0x100]);
		claims.packet(0x1ff);
		assert_eq!(claims.rows().count(), 0);
		claims.pat(&[(1, pid(0x20))], Some(0));
		claims.packet(0x1ff);
		claims.packet(0x200);
		assert_eq!(claims.rows().collect::<Vec<_>>(), [(0x1ff, 1, true), (0x200, 1, true)]);
		// A PMT claiming it again keeps its count but ends its dropped row.
		claims.pmt(1, [0x100, 0x200]);
		assert_eq!(claims.rows().collect::<Vec<_>>(), [(0x1ff, 1, true), (0x200, 1, false)]);
	}
}
