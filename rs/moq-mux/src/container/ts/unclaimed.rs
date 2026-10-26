//! The packets an import drops on PIDs no table claims.
//!
//! A PID is claimed by the PAT (PID 0 and each listed program's PMT PID), by a route an import
//! keeps (a PMT, an elementary stream, a clock), or by the SI the import captures. The null PID
//! is stuffing, rebuilt on export, and never counted. Each owner decides when counting starts:
//! until every listed PMT is in, a PID may belong to a program whose PMT is still on its way.

use std::collections::BTreeMap;

use mpeg2ts::ts::Pid;

use super::catalog::SI_PIDS;
use super::stats;

/// TS packets that carry nothing: stuffing.
const NULL_PID: u16 = 0x1fff;

/// Packets dropped on each PID no table claims, kept across the PID's claims since counters
/// never go backward. The 13-bit PID namespace bounds it.
#[derive(Default)]
pub(super) struct Unclaimed {
	dropped: BTreeMap<u16, u64>,
}

impl Unclaimed {
	/// Whether `pid` is claimed whatever the PMTs say: the null PID, the PAT, and, when `si` is
	/// captured, the standalone SI PIDs.
	pub fn fixed(pid: u16, si: bool) -> bool {
		pid == NULL_PID || pid == Pid::PAT || (si && SI_PIDS.contains(&pid))
	}

	/// Count a packet dropped on `pid`, warning on its first.
	pub fn packet(&mut self, pid: u16) {
		let dropped = self.dropped.entry(pid).or_default();
		if *dropped == 0 {
			tracing::warn!(
				pid,
				"dropping a PID no imported table claims; carry the multiplex whole to keep it"
			);
		}
		*dropped += 1;
	}

	/// Fold the dropped packets into `streams`, counted in `refused`. A PID `claimed` says no
	/// table claims now names no track; one claimed again keeps its count in its route's row.
	pub fn report(&self, streams: &mut BTreeMap<u16, stats::Stream>, claimed: impl Fn(u16) -> bool) {
		for (&pid, &dropped) in &self.dropped {
			let row = streams
				.entry(pid)
				.or_insert_with(|| stats::Stream::new("", stats::Class::Data));
			row.refused += dropped;
			if !claimed(pid) {
				row.track.clear();
				row.class = stats::Class::Data;
			}
		}
	}
}
