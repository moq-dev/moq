//! Every program of a multiplex, each imported as its own broadcast.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Context;
use mpeg2ts::ts::Pid;

use super::demux::{Demux, Packet};
use super::health::{Checks, Health};
use super::import::SectionReassembler;
use super::unclaimed::Unclaimed;
use super::{Ext, Import, catalog::SI_PIDS, health, psi, stats};
use crate::catalog;

/// Imports every program the PAT lists, each as its own broadcast on `origin`, with its own
/// clock and catalog.
///
/// Program `n` of `event.hang` publishes on `event/n.hang`, keeping the catalog suffix last so
/// format detection still sees it; a name without one gains a plain `/n`. A program starts once
/// its PMT lists an elementary stream; until then it publishes nothing. One whose PMT has not
/// arrived 0.5 s of program clock after the PAT listed it, the bound TR 101 290 sets for
/// `PMT_error_2`, refuses the multiplex. The clock is a running program's, so nothing is late
/// before one runs, and one whose PMT arrived listing no stream yet waits. Input ahead of that
/// PMT is dropped, as a single [`Import`] drops media ahead of its PSI; its SI arrives with the
/// next repetition, as for an import joining mid-stream.
///
/// The PAT is followed as it changes. A program a later PAT adds waits for its PMT as the first
/// PAT's do, and each program follows its PMT to the PID the latest PAT names. One a later PAT
/// drops is retired: its tracks finish and its broadcast is unannounced, while the other
/// programs carry on. A program that returns publishes a new instance, under a new epoch.
///
/// The multiplex is parsed once: framing, continuity, the PAT and the SI sections are shared,
/// and each program reads only the packets on the PIDs its PAT entry and PMT claim. A packet on a
/// PID no program reads, the PAT's PMT PIDs, the SI PIDs and the null PID aside, is counted once
/// for the multiplex (see [`stats::Snapshot::streams`]). Counting waits until every program the
/// PAT lists has its PMT, since until then a PID may be a program's still to come.
///
/// Each PID is counted once, so a PID several programs read has one continuous set of
/// counters while they start and retire. The PMT sections are reassembled here, once per PMT
/// PID, so a bad CRC counts once however many programs read the PID. Every other count of a PID
/// is taken from one program at a time for each way it is read: its row and PTS
/// cadence from a program carrying it, or else one reading it, its PCR checks from one taking
/// its clock there, and its PMT interval from one reading its PMT there. When that program stops
/// reading it so, the earliest running one that does takes over the intervals it was timing,
/// and counts on from the total it leaves.
pub struct Programs {
	origin: moq_net::origin::Producer,
	name: moq_net::Path,
	config: catalog::Config,
	demux: Demux,
	/// The latest whole PAT, from the first that lists a program, which seeds every program.
	pat: Option<psi::Pat>,
	/// The programs the latest PAT lists and no PMT has started yet, by the PMT PID it names
	/// for them, with how long each has waited.
	waiting: HashMap<u16, HashMap<u16, Wait>>,
	/// The PMT sections on each PMT PID the latest PAT lists, reassembled once for every program.
	pmts: HashMap<u16, SectionReassembler>,
	/// Whether each PID, by index, is one of [`Self::pmts`], read without hashing every packet.
	pmt_pids: Vec<bool>,
	/// PMT sections dropped for a bad CRC, by PID.
	crc_errors: BTreeMap<u16, u64>,
	/// Each PID's counts, but for its PMT sections' CRC, by the way it is read.
	tallies: BTreeMap<(u16, Checks), Tally>,
	/// Empty until a PMT starts a program, and again once a PAT retires every one.
	programs: Vec<Program>,
	/// The programs reading each PID they claim, indexed by PID, in program order. The PAT and
	/// the SI PIDs go to every program instead.
	readers: Vec<Vec<usize>>,
	/// The programs the current [`decode`](Self::decode) call routed a packet to.
	touched: Vec<usize>,
	/// Whether a retired program had announced.
	announced: bool,
	/// Times the programs waiting for their PMT.
	clock: Clock,
	/// The [`Clock`] reading past which the longest-waiting program is late, while one waits.
	deadline: Option<u64>,
	/// The packets on PIDs no program reads.
	unclaimed: Unclaimed,
}

/// `PMT_error_2`: a PMT recurs at least every 0.5 s of program clock.
const PMT_LIMIT: u64 = super::passthrough::PCR_HZ / 2;

/// How long a listed program has waited for its PMT, on the [`Clock`].
#[derive(Clone, Copy)]
enum Wait {
	/// Since this many ticks.
	Since(u64),
	/// Its PMT arrived listing no stream yet, so it is not late.
	Described,
}

/// A program clock for the programs that have none yet: the ticks a running program's clock
/// has advanced while it was the reference, which passes to another when it retires. It stands
/// still while no running program has a clock, so nothing is late before one runs.
#[derive(Default)]
struct Clock {
	ticks: u64,
	/// The reference program's index, and its clock when last read.
	reference: Option<(usize, u64)>,
}

impl Clock {
	/// Take program `index` as the reference if there is none and its clock runs.
	fn offer(&mut self, index: usize, import: &Import<Ext>) {
		if self.reference.is_none() {
			self.reference = import.now().map(|now| (index, now));
		}
	}

	/// Advance by what the reference program's clock moved since the last read.
	fn advance(&mut self, programs: &[Program]) {
		if let Some((index, last)) = &mut self.reference
			&& let Some(now) = programs[*index].import.now()
		{
			self.ticks += now.saturating_sub(*last);
			*last = now;
		}
	}
}

/// One program's broadcast.
struct Program {
	/// The PAT's program number.
	number: u16,
	/// Owns announcement control while the importer holds a fill clone.
	broadcast: moq_net::origin::Publication,
	import: Import<Ext>,
	catalog: catalog::Producer<Ext>,
	/// The route and catalog track until the catalog first names a servable track: announced any
	/// sooner, a relay would ask for tracks it cannot serve yet.
	pending: Option<(moq_net::origin::Route, moq_net::track::Consumer)>,
	/// The PIDs this program is listed under in [`Programs::readers`], sorted.
	claims: Vec<u16>,
	/// The ways it reads each PID, sorted, but for the PAT and the SI PIDs every program reads.
	reads: Vec<(u16, Checks)>,
	/// Whether the current [`Programs::decode`] call routed it a packet.
	touched: bool,
}

impl Program {
	/// Announce the broadcast once its catalog has gone out.
	fn announce_if_listed(&mut self) -> anyhow::Result<bool> {
		let Some((route, listed)) = &mut self.pending else {
			return Ok(true);
		};
		if !matches!(
			listed.poll_largest(&kio::Waiter::noop()),
			std::task::Poll::Ready(Some(_))
		) {
			return Ok(false);
		}
		self.broadcast
			.announce(route.clone())
			.with_context(|| format!("failed to announce program {}", self.number))?;
		self.pending = None;
		Ok(true)
	}
}

/// What one program counted on one PID in one way: for [`Checks::Stream`] its row too.
#[derive(Clone, Default)]
struct Count {
	row: Option<stats::Stream>,
	errors: health::Errors,
}

impl Count {
	/// `pid`'s `checks` in a program's `stats` and `errors`.
	fn read(stats: &stats::Snapshot, errors: &health::Errors, (pid, checks): (u16, Checks)) -> Self {
		Self {
			row: (checks == Checks::Stream)
				.then(|| stats.streams.get(&pid).cloned())
				.flatten(),
			errors: errors.only(pid, checks),
		}
	}

	/// What these counts gained since `from`, an earlier reading of the same program.
	fn since(mut self, from: &Self) -> Self {
		if let (Some(row), Some(from)) = (&mut self.row, &from.row) {
			row.combine(from, u64::saturating_sub);
		}
		self.errors.combine(&from.errors, u64::saturating_sub);
		self
	}

	/// Add what `earlier` counted, keeping this row's track, class and quiet.
	fn after(mut self, earlier: &Self) -> Self {
		match (&mut self.row, &earlier.row) {
			(Some(row), Some(earlier)) => row.combine(earlier, u64::saturating_add),
			(None, Some(earlier)) => self.row = Some(earlier.clone()),
			_ => {}
		}
		self.errors.combine(&earlier.errors, u64::saturating_add);
		self
	}
}

/// A program's counts and timers just before it read a packet on its PMT PID, which may move
/// its counts to another program.
struct Before {
	stats: stats::Snapshot,
	errors: health::Errors,
	health: Health,
	now: Option<u64>,
}

impl Before {
	fn of(import: &Import<Ext>) -> Self {
		let (health, now) = import.timing();
		Self {
			stats: import.stats(),
			errors: import.errors().clone(),
			health: health.clone(),
			now,
		}
	}
}

/// One PID's counts in one way for the multiplex.
#[derive(Default)]
struct Tally {
	/// What the programs that counted it before the current one counted.
	kept: Count,
	/// The program counting it now, by index, with its reading when it took over.
	owner: Option<(usize, Count)>,
}

impl Tally {
	/// The counts, given the owner's reading now.
	fn total(&self, now: Option<Count>) -> Count {
		match (&self.owner, now) {
			(Some((_, from)), Some(now)) => now.since(from).after(&self.kept),
			_ => self.kept.clone(),
		}
	}

	/// The owner stops counting, its reading at the end `ended`.
	fn release(&mut self, ended: Count) {
		self.kept = self.total(Some(ended));
		self.owner = None;
		// Nothing is expected on a PID no program counts, so it is not quiet.
		if let Some(row) = &mut self.kept.row {
			row.quiet = None;
		}
	}
}

/// Whether every program reads `pid`: the PAT, and the SI PIDs.
fn everyone(pid: u16) -> bool {
	pid == Pid::PAT || SI_PIDS.contains(&pid)
}

/// The ways `import` reads the PIDs it `claims`, sorted.
fn reads(import: &Import<Ext>, claims: &[u16]) -> Vec<(u16, Checks)> {
	let mut reads: Vec<_> = (claims.iter().map(|&pid| (pid, Checks::Stream)))
		.chain(import.pcr_pid().map(|pid| (pid, Checks::Clock)))
		.chain(import.pmt_pid().map(|pid| (pid, Checks::Table)))
		.collect();
	reads.sort_unstable();
	reads
}

/// The `reads` of sorted `ours` not in sorted `theirs`.
fn difference(ours: &[(u16, Checks)], theirs: &[(u16, Checks)]) -> Vec<(u16, Checks)> {
	(ours.iter())
		.filter(|read| theirs.binary_search(read).is_err())
		.copied()
		.collect()
}

/// Which running program counts `pid` in the way `checks`, given the one that does now: one
/// carrying an elementary stream there counts its row before one only reading the PID, and
/// otherwise the current one carries on while it reads the PID so, then the earliest that does.
fn counter(
	programs: &[Program],
	readers: &[Vec<usize>],
	(pid, checks): (u16, Checks),
	current: Option<usize>,
) -> Option<usize> {
	let everyone = everyone(pid);
	let reads = |index: usize| everyone || programs[index].reads.binary_search(&(pid, checks)).is_ok();
	let candidates = || -> Box<dyn Iterator<Item = usize> + '_> {
		match everyone {
			true => Box::new(0..programs.len()),
			false => Box::new(readers[usize::from(pid)].iter().copied()),
		}
	};
	let current = current.filter(|&index| reads(index));
	if checks == Checks::Stream && !everyone {
		let carries = |index: usize| programs[index].import.carries(pid);
		if !current.is_some_and(carries)
			&& let Some(carrier) = candidates().find(|&index| carries(index))
		{
			return Some(carrier);
		}
	}
	current.or_else(|| candidates().find(|&index| reads(index)))
}

impl Programs {
	/// Split the multiplex into broadcasts named after `name`, each catalog built from `config`.
	pub fn new(origin: moq_net::origin::Producer, name: impl Into<moq_net::Path>, config: catalog::Config) -> Self {
		Self {
			origin,
			name: name.into(),
			config,
			demux: Demux::new(true),
			pat: None,
			waiting: HashMap::new(),
			pmts: HashMap::new(),
			pmt_pids: vec![false; 1 << 13],
			crc_errors: BTreeMap::new(),
			tallies: BTreeMap::new(),
			programs: Vec::new(),
			readers: vec![Vec::new(); 1 << 13],
			touched: Vec::new(),
			announced: false,
			clock: Clock::default(),
			deadline: None,
			unclaimed: Unclaimed::default(),
		}
	}

	/// Demux a chunk of the multiplex. A trailing partial packet is retained for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		let mut demux = std::mem::take(&mut self.demux);
		let result = self.decode_with(&mut demux, data);
		self.demux = demux;
		result
	}

	fn decode_with(&mut self, demux: &mut Demux, data: &[u8]) -> anyhow::Result<()> {
		demux.push(data);
		while let Some(packet) = demux.next() {
			if let Some(pat) = &packet.pat {
				self.follow(pat)?;
			} else if self.pmt_pids[usize::from(packet.pid)] {
				self.describe(&packet)?;
			}
			// Every program reads the PAT, and the SI sections the parser reassembled.
			let everyone = packet.pid == Pid::PAT || (SI_PIDS.contains(&packet.pid) && !packet.sections.is_empty());
			if !everyone && self.readers[usize::from(packet.pid)].is_empty() {
				self.unclaimed(&packet);
			}
			let readers = (!everyone).then(|| self.readers[usize::from(packet.pid)].as_slice());
			let mut changed = Vec::new();
			for at in 0..readers.map_or(self.programs.len(), <[usize]>::len) {
				let index = readers.map_or(at, |readers| readers[at]);
				let program = &mut self.programs[index];
				if !std::mem::replace(&mut program.touched, true) {
					self.touched.push(index);
				}
				let import = &mut program.import;
				// A PMT moves counts between programs as it stood before the PMT, so a tail its
				// end flushes or a refusal on the way to a PID counts once, and an interval the
				// PMT stops timing carries on.
				let before = (self.pmt_pids[usize::from(packet.pid)] && import.pmt_pid() == Some(packet.pid))
					.then(|| Before::of(import));
				import.packet(&packet)?;
				self.clock.offer(index, import);
				if import.claims_changed() {
					changed.push((index, before));
				}
			}
			drop(packet);
			// Only a PAT or PMT moves claims or restarts a route.
			for (index, before) in changed {
				for pid in self.programs[index].import.restarts() {
					// Another program still reading the PID judges its partial bytes against
					// this chain, so only the PID's sole reader restarts it.
					if self.readers[usize::from(pid)].iter().all(|&reader| reader == index) {
						demux.restart(pid);
					}
				}
				self.reclaim(index, before.as_ref());
			}
			// The waiting programs have no clock of their own until their PMT names one.
			self.clock.advance(&self.programs);
			if self.deadline.is_some_and(|deadline| self.clock.ticks > deadline) {
				return Err(self.late());
			}
		}
		demux.drain();
		// Only a program that read something has anything to flush or announce, so a call
		// costs what it carried rather than one pass over every program.
		for index in self.touched.drain(..) {
			let program = &mut self.programs[index];
			program.touched = false;
			program.import.flush_batch()?;
			program.announce_if_listed()?;
		}
		Ok(())
	}

	/// Follow a whole PAT, from the first that lists a program: retire each started program it
	/// no longer lists, move each waiting one to the PMT PID it names or drop it, and wait for
	/// the PMT of each it adds. A started program follows its own PMT PID.
	///
	/// Fails on a PAT naming another transport stream while a program runs, as each program's
	/// importer would, and when a retired program cannot finish.
	fn follow(&mut self, pat: &psi::Pat) -> anyhow::Result<()> {
		// The PAT repeats several times a second, mostly unchanged.
		if self.pat.as_ref() == Some(pat) {
			return Ok(());
		}
		if let Some(running) = self.pat.as_ref().filter(|_| !self.programs.is_empty()) {
			anyhow::ensure!(
				running.transport_stream_id == pat.transport_stream_id,
				"the PAT changed transport_stream_id from {} to {} while its programs run, which their carried service information does not describe",
				running.transport_stream_id,
				pat.transport_stream_id
			);
		}
		let mut listed = HashMap::new();
		for entry in &pat.programs {
			// An importer takes a program's first entry, so a repeated number waits there alone.
			if entry.program_number != 0 {
				listed.entry(entry.program_number).or_insert(entry.pmt_pid.as_u16());
			}
		}
		if self.pat.is_none() && listed.is_empty() {
			return Ok(());
		}
		self.retire(&listed)?;
		let mut moved = Vec::new();
		self.waiting.retain(|pid, waiting| {
			waiting.retain(|number, _| match listed.get(number) {
				Some(listed) if listed == pid => true,
				Some(&listed) => {
					moved.push((*number, listed));
					false
				}
				None => false,
			});
			!waiting.is_empty()
		});
		// A PMT on a new PID is due 0.5 s from the PAT that names it.
		let now = Wait::Since(self.clock.ticks);
		for (number, pid) in moved {
			self.waiting.entry(pid).or_default().insert(number, now);
		}
		let known: HashSet<u16> = self
			.programs
			.iter()
			.map(|program| program.number)
			.chain(self.waiting.values().flat_map(|waiting| waiting.keys().copied()))
			.collect();
		for (&number, &pid) in &listed {
			if known.contains(&number) {
				continue;
			}
			if self.pat.is_some() {
				tracing::info!(program = number, pmt_pid = pid, "TS program joined the PAT");
			}
			self.waiting.entry(pid).or_default().insert(number, now);
		}
		// A PMT PID the PAT now names starts reassembling at its next section.
		let named: HashSet<u16> = listed.values().copied().collect();
		for &pid in self.pmts.keys() {
			self.pmt_pids[usize::from(pid)] = false;
		}
		self.pmts.retain(|pid, _| named.contains(pid));
		for pid in named {
			self.pmts.entry(pid).or_default();
			self.pmt_pids[usize::from(pid)] = true;
		}
		self.pat = Some(pat.clone());
		self.time_waits();
		Ok(())
	}

	/// Time the longest wait for a PMT, once the programs waiting have changed.
	fn time_waits(&mut self) {
		self.deadline = self
			.waiting
			.values()
			.flat_map(HashMap::values)
			.filter_map(|wait| match wait {
				Wait::Since(since) => Some(since + PMT_LIMIT),
				Wait::Described => None,
			})
			.min();
	}

	/// The refusal once a listed program has waited more than 0.5 s of program clock for its
	/// PMT, naming the programs that have.
	fn late(&self) -> anyhow::Error {
		let mut late: Vec<u16> = self
			.waiting
			.values()
			.flat_map(|waiting| waiting.iter())
			.filter(|(_, wait)| matches!(wait, Wait::Since(since) if self.clock.ticks > since + PMT_LIMIT))
			.map(|(&number, _)| number)
			.collect();
		late.sort_unstable();
		anyhow::anyhow!(
			"{} TS programs the PAT lists sent no PMT within 0.5 s of program clock, the TR 101 290 PMT_error_2 bound: {:?}",
			late.len(),
			&late[..late.len().min(10)]
		)
	}

	/// End every started program `listed` no longer names: finish its tracks, announcing it
	/// as the end of the input would, then its catalog, and close its broadcast, so it is
	/// unannounced now rather than lingering. Subscribers already reading a track read it to
	/// its end, and the other programs carry on. Each PID a retired program counted stays
	/// counted, and its next reader counts on from there.
	///
	/// The programs and the PIDs they read are rebuilt once, so a PAT costs what the programs
	/// claim however many it drops. Fails when a program cannot finish, as an import does at
	/// the end of its input.
	fn retire(&mut self, listed: &HashMap<u16, u16>) -> anyhow::Result<()> {
		let leaving = |program: &Program| !listed.contains_key(&program.number);
		if !self.programs.iter().any(leaving) {
			return Ok(());
		}
		// What each leaver had counted before its finish, by index.
		let before: HashMap<usize, (stats::Snapshot, health::Errors)> = (self.programs.iter().enumerate())
			.filter(|(_, program)| leaving(program))
			.map(|(index, program)| (index, (program.import.stats(), program.import.errors().clone())))
			.collect();
		// A refusal leaves every program listed, so the caller's abort reaches them all.
		for program in self.programs.iter_mut().filter(|program| leaving(program)) {
			tracing::info!(
				program = program.number,
				"TS program left the PAT; retiring its broadcast"
			);
			program.import.finish()?;
			self.announced |= program.announce_if_listed()?;
			program.catalog.finish()?;
		}
		let mut kept = 0;
		let moved: Vec<Option<usize>> = self
			.programs
			.iter()
			.map(|program| {
				(!leaving(program)).then(|| {
					kept += 1;
					kept - 1
				})
			})
			.collect();
		for program in &self.programs {
			for &pid in &program.claims {
				self.readers[usize::from(pid)].clear();
			}
		}
		let mut gone = HashMap::new();
		for (index, program) in std::mem::take(&mut self.programs).into_iter().enumerate() {
			match moved[index] {
				Some(_) => self.programs.push(program),
				None => {
					gone.insert(index, (program, None));
				}
			}
		}
		for (index, program) in self.programs.iter().enumerate() {
			for &pid in &program.claims {
				self.readers[usize::from(pid)].push(index);
			}
		}
		self.touched.retain_mut(|index| match moved[*index] {
			Some(to) => {
				*index = to;
				true
			}
			None => false,
		});
		// A clock that leaves stops here; the next running one carries on from this reading.
		self.clock.reference = self
			.clock
			.reference
			.and_then(|(index, last)| Some((moved[index]?, last)));
		// Each count a leaver kept passes to the next program reading the PID so. One a running
		// program carrying the PID takes ends at the leaver's count before its finish: that
		// program holds the tail the finish flushed, and counts it when it delivers it.
		let mut readings = HashMap::new();
		for (&read, tally) in &mut self.tallies {
			let Some((owner, _)) = &mut tally.owner else {
				continue;
			};
			if let Some(to) = moved[*owner] {
				*owner = to;
				continue;
			}
			let (program, after) = gone.get_mut(owner).expect("a leaver counted it");
			let next = counter(&self.programs, &self.readers, read, None);
			let holds_tail =
				read.1 == Checks::Stream && next.is_some_and(|next| self.programs[next].import.carries(read.0));
			let ended = match holds_tail {
				true => {
					let (stats, errors) = &before[owner];
					Count::read(stats, errors, read)
				}
				false => {
					let stats = after.get_or_insert_with(|| program.import.stats());
					Count::read(stats, program.import.errors(), read)
				}
			};
			tally.release(ended);
			if let Some(next) = next {
				let to = &mut self.programs[next].import;
				to.take_over(read.0, program.import.timing());
				let reading = readings.entry(next).or_insert_with(|| to.stats());
				tally.owner = Some((next, Count::read(reading, to.errors(), read)));
			}
		}
		for (program, _) in gone.values() {
			program.broadcast.close();
		}
		Ok(())
	}

	/// Count a packet on a PID no program reads, unless the PAT claims it or a waiting program's
	/// PMT may still claim it.
	fn unclaimed(&mut self, packet: &Packet) {
		// A packet flagged corrupt may carry another PID's number.
		if packet.pkt[1] & 0x80 != 0 || Unclaimed::fixed(packet.pid, true) || self.pmt_pids[usize::from(packet.pid)] {
			return;
		}
		// A program still waiting for its PMT may claim it, and a late one refuses the multiplex.
		if self.pat.is_some() && self.deadline.is_none() {
			self.unclaimed.packet(packet.pid);
		}
	}

	/// Reassemble a packet on a PMT PID, count the PMT sections it completes with a bad CRC, and
	/// start each waiting program whose PMT it completes with a stream listed.
	///
	/// A started program reads this packet itself, from the reassembler as it stood before it,
	/// so its own PMT path applies the PMT and starts its clock from the PCR already seen. Of
	/// several revisions in the packet, the last decides.
	fn describe(&mut self, packet: &Packet) -> anyhow::Result<()> {
		let Some(sections) = self.pmts.get_mut(&packet.pid) else {
			return Ok(());
		};
		let mut waiting = self.waiting.get_mut(&packet.pid);
		let before = waiting.is_some().then(|| sections.clone());
		let mut completed = Vec::new();
		sections.push(packet.pkt, &mut completed);
		let mut corrupt = 0;
		let mut latest: Vec<psi::Pmt> = Vec::new();
		for section in completed {
			// Other tables may share the PID.
			if section.first() != Some(&psi::PMT_TABLE_ID) {
				continue;
			}
			if !psi::crc_ok(&section) {
				corrupt += 1;
				continue;
			}
			if let Some(waiting) = &waiting
				&& let Some(pmt) = psi::Pmt::parse(&section)
				&& waiting.contains_key(&pmt.program_number)
			{
				latest.retain(|seen| seen.program_number != pmt.program_number);
				latest.push(pmt);
			}
		}
		if corrupt > 0 {
			*self.crc_errors.entry(packet.pid).or_default() += corrupt;
			tracing::warn!(pid = packet.pid, "dropped a PSI section with a bad CRC");
		}
		let (Some(waiting), Some(before)) = (waiting.as_mut(), before) else {
			return Ok(());
		};
		let mut described = false;
		for pmt in latest.iter().filter(|pmt| pmt.streams.is_empty()) {
			described |= matches!(
				waiting.insert(pmt.program_number, Wait::Described),
				Some(Wait::Since(_))
			);
		}
		latest.retain(|pmt| !pmt.streams.is_empty());
		if latest.is_empty() {
			if described {
				self.time_waits();
			}
			return Ok(());
		}
		for pmt in &latest {
			waiting.remove(&pmt.program_number);
		}
		if waiting.is_empty() {
			self.waiting.remove(&packet.pid);
		}
		for pmt in latest {
			self.start(pmt.program_number, packet.pid, before.clone())?;
		}
		self.time_waits();
		Ok(())
	}

	/// Create the broadcast of program `number`, its importer seeded with the PAT and carrying
	/// on the PMT `sections` began on `pmt_pid`.
	fn start(&mut self, number: u16, pmt_pid: u16, sections: SectionReassembler) -> anyhow::Result<()> {
		let pat = self.pat.as_ref().expect("a program waits on its PAT");
		let name = program_broadcast(self.name.as_str(), number);
		let broadcast = self
			.origin
			.create_broadcast(&name)
			.with_context(|| format!("failed to create broadcast {name}"))?;
		// TS carries undecoded elementary streams verbatim, so each catalog carries the
		// `mpegts` extension, and owns its broadcast's catalog tracks.
		let config = self
			.config
			.clone()
			.with_catalog(catalog::hang::Catalog::<Ext>::default());
		let catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), config)?;
		let pmt_pid = Pid::new(pmt_pid).expect("a packet's PID fits 13 bits");
		let mut import = Import::new(broadcast.broadcast().clone(), catalog.reserve())
			.with_program(number)
			.with_pmt(pmt_pid, sections);
		import.handle_pat(pat)?;
		// Each program is its own publisher instance for this run, so a restart
		// replaces it rather than resuming into it.
		let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
		let listed = broadcast.consume().track(hang::Catalog::DEFAULT_NAME)?;
		self.programs.push(Program {
			number,
			broadcast,
			import,
			catalog,
			pending: Some((route, listed)),
			claims: Vec::new(),
			reads: Vec::new(),
			touched: false,
		});
		let index = self.programs.len() - 1;
		self.reclaim(index, None);
		let mut readings = HashMap::new();
		for pid in std::iter::once(Pid::PAT).chain(SI_PIDS.iter().copied()) {
			self.settle((pid, Checks::Stream), index, None, &mut readings);
		}
		Ok(())
	}

	/// Move program `index` in [`Self::readers`] to the PIDs it claims now, and settle who
	/// counts each PID in each way it reads it now or did, given its counts `before` the packet
	/// that moved them, if that was a PMT.
	fn reclaim(&mut self, index: usize, before: Option<&Before>) {
		let program = &mut self.programs[index];
		let claims = program.import.claims();
		for &pid in program.claims.iter().filter(|pid| claims.binary_search(pid).is_err()) {
			self.readers[usize::from(pid)].retain(|&reader| reader != index);
		}
		for &pid in claims.iter().filter(|pid| program.claims.binary_search(pid).is_err()) {
			let readers = &mut self.readers[usize::from(pid)];
			let at = readers.partition_point(|&reader| reader < index);
			readers.insert(at, index);
		}
		let reads = reads(&program.import, &claims);
		program.claims = claims;
		let mut settled = difference(&program.reads, &reads);
		settled.extend_from_slice(&reads);
		program.reads = reads;
		let mut readings = HashMap::new();
		for read in settled {
			self.settle(read, index, before, &mut readings);
		}
	}

	/// Move `read`'s count to the program [`counter`] picks, if not the one counting it now. The
	/// next counts on from its reading now, and the last counts up to its reading now, but for
	/// program `index`, which read the packet that moved the count: its reading `before` it
	/// stands where it starts counting, so what that packet counted on the way to the PID counts,
	/// and where it stops in favour of a program carrying the PID, which holds the tail its
	/// route's end flushed. `readings` caches the programs' stats.
	fn settle(
		&mut self,
		read: (u16, Checks),
		index: usize,
		before: Option<&Before>,
		readings: &mut HashMap<usize, stats::Snapshot>,
	) {
		let tally = self.tallies.entry(read).or_default();
		let current = tally.owner.as_ref().map(|&(owner, _)| owner);
		let next = counter(&self.programs, &self.readers, read, current);
		if next == current {
			return;
		}
		let programs = &self.programs;
		let mut reading = |at: usize, earlier: bool| match before.filter(|_| earlier && at == index) {
			Some(before) => Count::read(&before.stats, &before.errors, read),
			None => {
				let import = &programs[at].import;
				let stats = readings.entry(at).or_insert_with(|| import.stats());
				Count::read(stats, import.errors(), read)
			}
		};
		if let Some(current) = current {
			let holds_tail = read.1 == Checks::Stream && next.is_some_and(|next| programs[next].import.carries(read.0));
			tally.release(reading(current, holds_tail));
		}
		let Some(next) = next else {
			return;
		};
		tally.owner = Some((next, reading(next, true)));
		let Some(current) = current else {
			return;
		};
		match before.filter(|_| current == index) {
			Some(before) => (self.programs[next].import).take_over(read.0, (&before.health, before.now)),
			None => {
				let [from, to] = self
					.programs
					.get_disjoint_mut([current, next])
					.expect("a count moves to another program");
				to.import.take_over(read.0, from.import.timing());
			}
		}
	}

	/// Every program's counters in one map, each PID counted once.
	///
	/// The multiplex is graded once. Programs sharing a PMT PID read the same PMT sections, so
	/// a dropped section counts once however many read it, and a PID several programs read is
	/// counted by one of them at a time. What a retired program counted stays in the totals, so
	/// no total goes back when a program leaves or returns. So are the packets on PIDs no
	/// program reads, in the row of the program that reads the PID whenever one does.
	pub fn stats(&self) -> stats::Snapshot {
		let mut readings: Vec<Option<stats::Snapshot>> = (0..self.programs.len()).map(|_| None).collect();
		let mut errors = self.demux.errors().clone();
		let mut stats = stats::Snapshot::default();
		for (&read, tally) in &self.tallies {
			let now = tally.owner.as_ref().map(|&(index, _)| {
				let import = &self.programs[index].import;
				let reading = readings[index].get_or_insert_with(|| import.stats());
				Count::read(reading, import.errors(), read)
			});
			let count = tally.total(now);
			if let Some(row) = count.row {
				stats.streams.insert(read.0, row);
			}
			errors.combine(&count.errors, u64::saturating_add);
		}
		self.unclaimed.report(&mut stats.streams, |pid| {
			Unclaimed::fixed(pid, true) || self.pmt_pids[usize::from(pid)] || !self.readers[usize::from(pid)].is_empty()
		});
		stats.crc_error = self.crc_errors.values().sum::<u64>() + self.demux.pat_crc_error();
		errors.report(&mut stats);
		stats
	}

	/// Finish each program's tracks, then its catalog while it still lists them.
	///
	/// Returns each running program's broadcast; a retired one has already closed. The finished
	/// tracks stay servable while this importer is held, so hand both to a
	/// [`Linger`](crate::Linger) to keep them up for late subscribers. Fails when the input
	/// ended before its PAT listed a program or any program declared a track.
	pub fn finish(&mut self) -> anyhow::Result<Vec<moq_net::broadcast::Producer>> {
		self.demux.finish();
		anyhow::ensure!(self.pat.is_some(), "input ended before its PAT listed a program");
		let mut announced = self.announced;
		for program in &mut self.programs {
			program.import.finish()?;
			announced |= program.announce_if_listed()?;
			program.catalog.finish()?;
		}
		anyhow::ensure!(announced, "input ended before any program declared a track");
		Ok(self
			.programs
			.iter()
			.map(|program| program.broadcast.broadcast().clone())
			.collect())
	}

	/// Abort every program's catalog and media tracks with `err`. Consumes the importer.
	pub fn abort(self, err: moq_net::Error) {
		for program in self.programs {
			program.catalog.abort(err.clone());
			program.import.abort(err.clone());
		}
	}
}

/// The broadcast one program of `name` publishes on: `event.hang` becomes `event/2.hang`.
fn program_broadcast(name: &str, program: u16) -> String {
	let (stem, extension) = match catalog::CatalogFormat::detect(name) {
		Some(format) => (&name[..name.len() - format.extension().len()], format.extension()),
		None => (name, ""),
	};
	// A final segment that is only the extension (`room/.hang`) leaves the stem's separator behind.
	match stem.strip_suffix('/').unwrap_or(stem) {
		"" => format!("{program}{extension}"),
		stem => format!("{stem}/{program}{extension}"),
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;
	use crate::catalog::hang::Container;
	use crate::container::Consumer;
	use crate::container::ts::fixture::{Flags, MS, Mux};
	use crate::container::ts::import::test::{
		CUE, MP2, annexb_au, corrupt, fifty_programs, mp2_pes, pat_section, pmt_section, psi_packets, two_programs,
		two_section_pat, video_pes,
	};

	/// The epoch each running program publishes under, announced yet or not.
	fn epochs(programs: &Programs) -> Vec<(u16, moq_net::Epoch)> {
		programs
			.programs
			.iter()
			.map(|program| {
				let route = program.broadcast.route();
				let route = route.as_ref().or(program.pending.as_ref().map(|(route, _)| route));
				(
					program.number,
					route.and_then(|route| route.epoch.clone()).expect("an epoch"),
				)
			})
			.collect()
	}

	/// Steps of a two-program multiplex: each a PAT listing `listed`, both programs' PMTs, and a
	/// PCR and an MP2 PES of two frames on each program, whether or not the PAT lists it.
	struct Steps {
		mux: Mux,
		cc: [u8; 2],
		pts: u64,
	}

	impl Steps {
		fn new() -> Self {
			Self {
				mux: Mux::default(),
				cc: [0; 2],
				pts: 90_000,
			}
		}

		fn step(&mut self, programs: &mut Programs, listed: &[(u16, u16)]) {
			self.mux
				.pat(listed)
				.pmt(0x100, 1, 0x1f1, &[(MP2, 0x61)], 0)
				.pmt(0x200, 2, 0x1f2, &[(MP2, 0x71)], 0);
			for (cc, (pid, clock)) in self.cc.iter_mut().zip([(0x61, 0x1f1), (0x71, 0x1f2)]) {
				self.mux.packet(clock, Flags::pcr(self.pts * 300));
				self.mux.out.extend(mp2_pes(pid, *cc, self.pts, [pid as u8; 2]));
				*cc = (*cc + 1) & 0x0f;
			}
			// Past both 72 ms frames.
			self.pts += 18_000;
			programs.decode(&std::mem::take(&mut self.mux.out)).unwrap();
		}
	}

	/// A program a later PAT adds publishes, one it drops retires alone, its catalog finished and
	/// its broadcast closed, and one it restores publishes a new instance that counts on from the
	/// retired one, while the program no PAT touched runs throughout.
	#[tokio::test]
	async fn programs_follow_the_pat() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut steps = Steps::new();

		steps.step(&mut programs, &[(1, 0x100)]);
		let one = epochs(&programs);
		assert_eq!(one.len(), 1);

		steps.step(&mut programs, &[(1, 0x100), (2, 0x200)]);
		let both = epochs(&programs);
		assert_eq!(both[0], one[0], "program 1 carries on");
		assert_eq!(both[1].0, 2, "the added program publishes");
		let two = programs.programs[1].broadcast.consume();
		let mut catalog = hang::catalog::Catalog::<()>::subscribe(&two).await.unwrap();

		steps.step(&mut programs, &[(1, 0x100)]);
		assert_eq!(epochs(&programs), one, "program 1 carries on");
		assert!(two.is_closed(), "the dropped program is unannounced");
		while catalog.next().await.unwrap().is_some() {}
		let retired = &programs.stats().streams[&0x71];
		assert_eq!(retired.units, 2, "the retired program's frames stay counted");
		assert_eq!(retired.quiet, None, "nothing is expected on a retired PID");

		steps.step(&mut programs, &[(1, 0x100), (2, 0x200)]);
		let back = epochs(&programs);
		assert_eq!(back[0], one[0], "program 1 carries on");
		assert_eq!(back[1].0, 2);
		assert_ne!(back[1].1, both[1].1, "the returning program is a new instance");

		programs.finish().unwrap();
		let stats = programs.stats();
		assert_eq!(stats.streams[&0x61].units, 8, "program 1 delivered every frame");
		assert_eq!(stats.streams[&0x71].units, 4, "the returning program counts on");
	}

	/// A PAT that drops a program reaches no importer of it, so the multiplex carries on rather
	/// than ending on that program's refusal, and the program after it keeps reading its PIDs.
	#[test]
	fn a_dropped_program_does_not_end_the_multiplex() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut steps = Steps::new();
		steps.step(&mut programs, &[(1, 0x100), (2, 0x200)]);
		// Fed a packet at a time, so the PAT that drops program 1 completes in a call of its own.
		steps.mux.pat(&[(2, 0x200)]);
		steps.mux.out.extend(mp2_pes(0x71, 1, 108_000, [0x71; 2]));
		for packet in std::mem::take(&mut steps.mux.out).chunks(188) {
			programs.decode(packet).unwrap();
		}
		let running: Vec<_> = epochs(&programs).into_iter().map(|(number, _)| number).collect();
		assert_eq!(running, [2]);
		programs.finish().unwrap();
		assert_eq!(programs.stats().streams[&0x71].units, 4, "program 2 read on");
	}

	/// A program the PAT lists whose PMT never arrives refuses the multiplex at the first packet
	/// past 0.5 s of program clock, timed on the program that runs, without waiting for another
	/// PAT; one whose PMT arrives listing no stream waits.
	#[test]
	fn a_program_without_a_pmt_is_refused_after_half_a_second() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut mux = Mux::default();
		mux.pat(&[(1, 0x100), (3, 0x300), (4, 0x400)]);
		let mut step = |programs: &mut Programs, ms: u64| {
			mux.pmt(0x100, 1, 0x1ff, &[(MP2, 0x61)], 0)
				.pmt(0x400, 4, 0x1ff, &[], 0)
				.packet(0x1ff, Flags::pcr(27_000_000 + ms * MS));
			programs.decode(&std::mem::take(&mut mux.out))
		};
		// Program 1's clock starts at its first PCR, and the wait with it.
		for ms in (0..=500).step_by(100) {
			step(&mut programs, ms).unwrap();
		}
		let err = step(&mut programs, 501).unwrap_err();
		assert!(format!("{err:#}").ends_with("bound: [3]"), "{err:#}");
	}

	/// A program leaving the PAT flushes the picture it shares with a program still running,
	/// which that program delivers itself at the next picture, so the picture counts once.
	#[test]
	fn a_picture_a_retired_program_flushed_counts_once() {
		const H264: u8 = 0x1b;
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut mux = Mux::default();
		let both = [(1, 0x100), (2, 0x200)];
		mux.pat(&both)
			.pmt(0x100, 1, 0x50, &[(H264, 0x50)], 0)
			.pmt(0x200, 2, 0x50, &[(H264, 0x50)], 0);
		let mut pictures =
			(0..).map(|n: u8| video_pes(0x50, n & 0x0f, 90_000 + u64::from(n) * 3_000, None, &annexb_au(n == 0)));
		mux.out.extend(pictures.next().unwrap());
		mux.out.extend(pictures.next().unwrap());
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		let units = |programs: &Programs| programs.stats().streams[&0x50].units;
		assert_eq!(units(&programs), 1, "the second picture ends at the third");

		mux.pat(&both[1..]);
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(programs.programs.len(), 1);
		assert_eq!(units(&programs), 1, "program 2 still holds the second picture");
		programs.decode(&pictures.next().unwrap()).unwrap();
		assert_eq!(units(&programs), 2, "the second picture counts once");
		programs.finish().unwrap();
		assert_eq!(units(&programs), 3);
	}

	/// A program leaving the PAT counts the picture its finish flushes on a PID the programs
	/// still running no longer carry, though one keeps its clock and its row there.
	#[test]
	fn a_picture_a_retired_program_flushed_alone_counts() {
		const H264: u8 = 0x1b;
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut mux = Mux::default();
		let both = [(1, 0x100), (2, 0x200)];
		mux.pat(&both)
			.pmt(0x100, 1, 0x50, &[(H264, 0x50)], 0)
			.pmt(0x200, 2, 0x50, &[(H264, 0x50)], 0);
		let mut pictures =
			(0..).map(|n: u8| video_pes(0x50, n & 0x0f, 90_000 + u64::from(n) * 3_000, None, &annexb_au(n == 0)));
		mux.out.extend(pictures.next().unwrap());
		mux.out.extend(pictures.next().unwrap());
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		let units = |programs: &Programs| programs.stats().streams[&0x50].units;
		assert_eq!(units(&programs), 1, "the second picture ends at the third");

		// Program 2 stops carrying the video, and flushes the second picture as it does.
		mux.pmt_at(1, 0x200, 2, 0x50, &[(MP2, 0x61, &[])], 0);
		mux.out.extend(pictures.next().unwrap());
		mux.out.extend(pictures.next().unwrap());
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(units(&programs), 3, "program 1 delivers the second and third");

		mux.pat(&both[1..]);
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(programs.programs.len(), 1);
		assert_eq!(units(&programs), 4, "program 1's finish delivers the fourth");
	}

	/// A PMT section a program began on a PID the PAT then moves it from ends there unread by
	/// any program, so a bad CRC on it counts for the programs still waiting on that PID.
	#[test]
	fn a_section_a_moved_program_left_unfinished_counts() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut cc = [0; 2];
		let mut data = psi_packets(0, &mut cc[0], &[], &pat_section(&[(1, 0x0100), (2, 0x0100)]));
		data.extend(psi_packets(
			0x0100,
			&mut cc[1],
			&[],
			&pmt_section(1, 0, &[(MP2, 0x0061, &[])]),
		));
		let long = corrupt(pmt_section(2, 0, &[(MP2, 0x0062, &[0x80; 200])]));
		let long = psi_packets(0x0100, &mut cc[1], &[], &long);
		assert_eq!(long.len(), 2 * 188);
		data.extend(&long[..188]);
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 1);

		let mut data = psi_packets(0, &mut cc[0], &[], &pat_section(&[(1, 0x0200), (2, 0x0100)]));
		data.extend(&long[188..]);
		programs.decode(&data).unwrap();
		assert_eq!(programs.stats().crc_error, 1);
	}

	/// A retired program's counts stay in the totals, once: a PMT PID it shared with a program
	/// still running is that program's to count, and its own PMT PID keeps its count.
	#[test]
	fn a_retired_program_keeps_what_it_alone_counted() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let pmt = |program: u16| pmt_section(program, 0, &[(MP2, 0x0060 + program, &[])]);
		let mut cc = [0; 3];
		let listed = pat_section(&[(1, 0x0100), (2, 0x0100), (3, 0x0300)]);
		let mut data = psi_packets(0, &mut cc[0], &[], &listed);
		let mut shared = pmt(1);
		shared.extend(pmt(2));
		data.extend(psi_packets(0x0100, &mut cc[1], &[], &shared));
		data.extend(psi_packets(0x0300, &mut cc[2], &[], &pmt(3)));
		data.extend(psi_packets(0x0100, &mut cc[1], &[], &corrupt(pmt(2))));
		data.extend(psi_packets(0x0300, &mut cc[2], &[], &corrupt(pmt(3))));
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 3);
		assert_eq!(programs.stats().crc_error, 2);

		programs
			.decode(&psi_packets(0, &mut cc[0], &[], &pat_section(&[(1, 0x0100)])))
			.unwrap();
		assert_eq!(programs.programs.len(), 1);
		assert_eq!(programs.stats().crc_error, 2, "each dropped section still counts once");
	}

	/// Programs sharing an elementary PID count its units once, each from its own start, so the
	/// earliest reader's count stands, while they all run and as they leave.
	#[test]
	fn programs_sharing_a_stream_count_it_once() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let all = [(1, 0x100), (2, 0x200), (3, 0x300)];
		let mut mux = Mux::default();
		// Each PMT arrives a PES after the one before, so each program counts less.
		for (cc, &(program, pid)) in (0..).zip(&all) {
			mux.pat(&all).pmt(pid, program, 0x61, &[(MP2, 0x61)], 0);
			mux.out
				.extend(mp2_pes(0x61, cc, 90_000 + u64::from(cc) * 18_000, [0x61; 2]));
			programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		}
		assert_eq!(programs.programs.len(), 3);
		assert_eq!(programs.stats().streams[&0x61].units, 6);
		for listed in [&all[1..], &all[2..]] {
			mux.pat(listed);
			programs.decode(&std::mem::take(&mut mux.out)).unwrap();
			assert_eq!(programs.programs.len(), listed.len());
			assert_eq!(programs.stats().streams[&0x61].units, 6, "{} running", listed.len());
		}
	}

	/// A PTS outage on a shared PID counts the same whether or not the program timing it retires
	/// or drops the PID partway: the program reading on takes over the interval running since
	/// the last PTS, which it never saw and which began before its own clock started, rather
	/// than timing the PID only from its own next PTS.
	#[test]
	fn a_pts_outage_counts_on_across_a_handover() {
		#[derive(PartialEq)]
		enum Leave {
			Stay,
			Retire,
			Drop,
		}
		let outage = |leave: Leave| {
			let origin = crate::source::produce_origin();
			let mut programs = Programs::new(
				origin,
				moq_net::Path::from_static("event.hang"),
				catalog::Config::default(),
			);
			let both = [(1, 0x100), (2, 0x200)];
			let mut mux = Mux::default();
			mux.pat(&both)
				.pmt(0x100, 1, 0x1ff, &[(MP2, 0x61)], 0)
				.packet(0x1ff, Flags::pcr(27_000_000));
			mux.out.extend(mp2_pes(0x61, 0, 90_000, [0x61; 2]));
			// Program 2 starts 0.5 s after the last PTS on the PID they share, and program 1
			// leaves before that PTS is late.
			for ms in (100..=1_500).step_by(100) {
				let left = ms >= 700;
				let listed = if left && leave == Leave::Retire {
					&both[1..]
				} else {
					&both[..]
				};
				mux.pat(listed);
				match left && leave == Leave::Drop {
					true => mux.pmt_at(1, 0x100, 1, 0x1ff, &[(MP2, 0x62, &[])], 0),
					false => mux.pmt(0x100, 1, 0x1ff, &[(MP2, 0x61)], 0),
				};
				if ms >= 500 {
					mux.pmt(0x200, 2, 0x1ff, &[(MP2, 0x61)], 0);
				}
				mux.packet(0x1ff, Flags::pcr(27_000_000 + ms * MS));
				programs.decode(&std::mem::take(&mut mux.out)).unwrap();
			}
			assert_eq!(programs.programs.len(), if leave == Leave::Retire { 1 } else { 2 });
			programs.stats().streams[&0x61].pts_error
		};
		assert_eq!(outage(Leave::Stay), 2, "the 1.5 s since the PTS");
		assert_eq!(outage(Leave::Retire), 2, "counted on by the program left");
		assert_eq!(outage(Leave::Drop), 2, "counted on by the program still carrying it");
	}

	/// A section stream refused for want of a PCR keeps its refusal in the totals once, though
	/// the PMT that adds it counts it before its program reads the PID, while the programs
	/// listing it run and as one leaves.
	#[test]
	fn a_stream_refused_without_a_pcr_counts_once() {
		const SECTIONS: u8 = 0x05;
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let both = [(1, 0x100), (2, 0x200)];
		let mut mux = Mux::default();
		for listed in [&both[..], &both[1..]] {
			mux.pat(listed);
			for (program, pid) in both {
				mux.pmt(pid, program, 0x1fff, &[(MP2, 0x61), (SECTIONS, 0x72)], 0);
			}
			programs.decode(&std::mem::take(&mut mux.out)).unwrap();
			assert_eq!(programs.programs.len(), listed.len());
			assert_eq!(programs.stats().streams[&0x72].refused, 1, "{} running", listed.len());
		}
	}

	/// A PID one program takes its clock from and a later one carries counts the carrier's
	/// units and the clock's PCR checks, each from the program reading it that way.
	#[test]
	fn a_pid_read_as_a_clock_and_a_stream_counts_both() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut mux = Mux::default();
		let both = [(1, 0x100), (2, 0x200)];
		mux.pat(&both).pmt(0x100, 1, 0x61, &[(MP2, 0x62)], 0);
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		mux.pat(&both).pmt(0x200, 2, 0x1f2, &[(MP2, 0x61)], 0);
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(programs.programs.len(), 2);
		// PCRs every 50 ms but for a 150 ms gap on 0x61, which breaks the 100 ms bound once, and
		// a PES of two frames on each stream every 150 ms.
		let mut cc = 0;
		for ms in (0..=450).step_by(50) {
			let pcr = Flags::pcr(27_000_000 + ms * MS);
			if !(200..=250).contains(&ms) {
				mux.packet(0x61, pcr);
			}
			mux.packet(0x1f2, pcr);
			if ms % 150 == 0 {
				for pid in [0x61, 0x62] {
					mux.out.extend(mp2_pes(pid, cc, 90_000 + ms * 90, [pid as u8; 2]));
				}
				cc += 1;
			}
		}
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		programs.finish().unwrap();
		let stats = programs.stats();
		assert_eq!(stats.streams[&0x61].units, 8, "program 2 delivers every frame");
		assert_eq!(stats.pcr_repetition_error, 1, "program 1 grades its clock");
	}

	/// A PMT that drops a shared H.264 PID from the program counting it flushes the picture it
	/// held, which the program still carrying the PID delivers itself at the next picture, so
	/// the picture counts once.
	#[test]
	fn a_picture_a_dropped_route_flushed_counts_once() {
		const H264: u8 = 0x1b;
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut mux = Mux::default();
		mux.pat(&[(1, 0x100), (2, 0x200)])
			.pmt(0x100, 1, 0x1f1, &[(H264, 0x50)], 0)
			.pmt(0x200, 2, 0x1f2, &[(H264, 0x50)], 0)
			.packet(0x1f1, Flags::pcr(27_000_000))
			.packet(0x1f2, Flags::pcr(27_000_000));
		let mut pictures =
			(0..).map(|n: u8| video_pes(0x50, n & 0x0f, 90_000 + u64::from(n) * 3_000, None, &annexb_au(n == 0)));
		mux.out.extend(pictures.next().unwrap());
		mux.out.extend(pictures.next().unwrap());
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		let units = |programs: &Programs| programs.stats().streams[&0x50].units;
		assert_eq!(units(&programs), 1, "the second picture ends at the third");

		mux.pmt_at(1, 0x100, 1, 0x1f1, &[(MP2, 0x61, &[])], 0);
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(units(&programs), 1, "program 2 still holds the second picture");
		programs.decode(&pictures.next().unwrap()).unwrap();
		assert_eq!(units(&programs), 2, "the second picture counts once");
		programs.finish().unwrap();
		assert_eq!(units(&programs), 3);
	}

	/// One PAT dropping programs on either side of those it keeps rebuilds who reads each PID
	/// at once: the programs left read on, their own PIDs and the shared ones alike.
	#[test]
	fn a_pat_dropping_several_programs_keeps_the_rest_reading() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut mux = Mux::default();
		let step = |mux: &mut Mux, listed: &[(u16, u16)], cc: u8| {
			mux.pat(listed);
			for (program, pid) in [(1, 0x100), (2, 0x100), (3, 0x300), (4, 0x100)] {
				mux.pmt(pid, program, 0x61, &[(MP2, 0x61), (MP2, 0x70 + program)], 0);
			}
			for pid in [0x61, 0x71, 0x72, 0x73, 0x74] {
				mux.out
					.extend(mp2_pes(pid, cc, 90_000 + u64::from(cc) * 18_000, [pid as u8; 2]));
			}
		};
		step(&mut mux, &[(1, 0x100), (2, 0x100), (3, 0x300), (4, 0x100)], 0);
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(programs.programs.len(), 4);
		step(&mut mux, &[(2, 0x100), (4, 0x100)], 1);
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		programs.finish().unwrap();
		let running: Vec<_> = epochs(&programs).into_iter().map(|(number, _)| number).collect();
		assert_eq!(running, [2, 4]);
		let units = |pid| programs.stats().streams[&pid].units;
		assert_eq!([0x61, 0x72, 0x74].map(units), [4; 3], "the programs kept read on");
		assert_eq!(
			[0x71, 0x73].map(units),
			[2; 2],
			"the dropped programs' frames stay counted"
		);
	}

	/// A program that starts and leaves within one call is announced as it retires, as the
	/// same bytes fed in two calls announce it, so the input declared a track.
	#[test]
	fn a_program_that_starts_and_leaves_in_one_call_is_announced() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut mux = Mux::default();
		mux.pat(&[(1, 0x100)]).pmt(0x100, 1, 0x61, &[(MP2, 0x61)], 0);
		mux.out.extend(mp2_pes(0x61, 0, 90_000, [0x61; 2]));
		mux.pat(&[]);
		programs.decode(&mux.out).unwrap();
		assert!(programs.programs.is_empty());
		programs.finish().unwrap();
	}

	/// A PAT naming another transport stream is refused while a program runs, before it retires
	/// one, even when it lists none of them.
	#[test]
	fn a_pat_naming_another_transport_stream_is_refused() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut steps = Steps::new();
		steps.step(&mut programs, &[(1, 0x100)]);
		steps.mux.pat_in(2, &[(2, 0x200)]);
		let err = programs.decode(&std::mem::take(&mut steps.mux.out)).unwrap_err();
		assert!(format!("{err:#}").contains("transport_stream_id"), "{err:#}");
		assert_eq!(programs.programs.len(), 1, "a refused PAT retires nothing");
	}

	/// A dropped program that cannot finish refuses the multiplex, as at the end of the input,
	/// and stays listed for the abort that follows, so its subscribers see the cause.
	#[tokio::test(start_paused = true)]
	async fn a_retired_program_that_cannot_finish_refuses_the_multiplex() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		// Program 2 carries a clock on 0x71 and SCTE-35 on 0x72, and no PTS to place a cue by.
		let mut cuei = vec![0xe0, 0x71, 0xf0, 6, 0x05, 4];
		cuei.extend_from_slice(b"CUEI");
		cuei.extend_from_slice(&[0x02, 0xe0, 0x71, 0xf0, 0, 0x86, 0xe0, 0x72, 0xf0, 0]);
		let mut mux = Mux::default();
		mux.pat(&[(1, 0x100), (2, 0x200)])
			.pmt(0x100, 1, 0x61, &[(MP2, 0x61)], 0)
			.section(0x200, &psi::section(psi::PMT_TABLE_ID, 2, 0, 0, 0, &cuei))
			.packet(0x71, Flags::pcr(27_000_000))
			.section(0x72, &CUE);
		mux.out.extend(mp2_pes(0x61, 0, 90_000, [0x61; 2]));
		programs.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(programs.programs.len(), 2);
		let mut catalog = programs.programs[1].catalog.consume().unwrap();

		mux.pat(&[(1, 0x100)]);
		let err = programs.decode(&std::mem::take(&mut mux.out)).unwrap_err();
		assert!(format!("{err:#}").contains("without a media timestamp"), "{err:#}");
		programs.abort(moq_net::Error::Transport("decode failed".into()));
		let err = loop {
			match catalog.next().await {
				Ok(Some(_)) => {}
				other => break other.unwrap_err(),
			}
		};
		assert!(matches!(err, crate::Error::Moq(moq_net::Error::Transport(ref reason)) if reason == "decode failed"));
	}

	#[tokio::test(start_paused = true)]
	async fn internal_program_abort_preserves_the_catalog_error() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		programs.decode(&two_programs()).unwrap();
		let mut consumers: Vec<_> = programs.programs.iter().map(|p| p.catalog.consume().unwrap()).collect();
		programs.abort(moq_net::Error::Transport("decode failed".into()));
		for consumer in &mut consumers {
			let err = loop {
				match consumer.next().await {
					Ok(Some(_)) => {}
					other => break other.unwrap_err(),
				}
			};
			assert!(
				matches!(err, crate::Error::Moq(moq_net::Error::Transport(ref reason)) if reason == "decode failed")
			);
		}
	}

	#[test]
	fn program_broadcasts_keep_the_catalog_suffix_last() {
		assert_eq!(program_broadcast("event.hang", 2), "event/2.hang");
		assert_eq!(program_broadcast("demo/event.msf", 7), "demo/event/7.msf");
		assert_eq!(program_broadcast("event", 1), "event/1");
		assert_eq!(program_broadcast("", 1), "1");
		assert_eq!(program_broadcast(".hang", 2), "2.hang");
		assert_eq!(program_broadcast("room/.msf", 2), "room/2.msf");
	}

	/// The first whole PAT in a buffer, as the parser reads it.
	fn pat_programs(data: &[u8]) -> Option<Vec<u16>> {
		let mut demux = Demux::new(false);
		demux.push(data);
		while let Some(packet) = demux.next() {
			if let Some(pat) = packet.pat {
				return Some(pat.programs.iter().map(|entry| entry.program_number).collect());
			}
		}
		None
	}

	#[test]
	fn pat_programs_reads_the_first_whole_pat() {
		let data = two_programs();
		assert_eq!(pat_programs(&data), Some(vec![1, 2]));
		// A sync byte in leading junk is skipped; a truncated PAT is not a PAT yet.
		let mut shifted = vec![0x47, 0x40, 0x00, 0x10];
		shifted.extend_from_slice(&data);
		assert_eq!(pat_programs(&shifted), Some(vec![1, 2]));
		assert_eq!(pat_programs(&data[..100]), None);
	}

	/// A PAT too big for one packet is read whole, so the program listed in its second packet
	/// publishes, and the forty-nine without a PMT start nothing.
	#[tokio::test]
	async fn a_pat_spanning_two_packets_is_read_whole() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin.clone(),
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let input = fifty_programs();
		for chunk in input.chunks(100) {
			programs.decode(chunk).unwrap();
		}
		programs.finish().unwrap();
		assert_eq!(programs.programs.len(), 1, "only program 50 has a PMT");

		let consumer = origin.consume().request("event/50.hang", None).await.unwrap();
		let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog");
		assert_eq!(catalog.audio.renditions.len(), 1, "program 50 publishes its stream");
	}

	/// A corrupt PAT is counted and ignored; the good repetition after it is read without
	/// counting again.
	#[test]
	fn a_corrupt_pat_starts_no_program() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		// A null packet after each PAT confirms its sync byte.
		let mut null = vec![0x47, 0x1f, 0xff, 0x10];
		null.resize(188, 0xff);
		let mut cc = 0;
		let mut data = psi_packets(0, &mut cc, &[], &corrupt(pat_section(&[(1, 0x0100)])));
		data.extend_from_slice(&null);
		programs.decode(&data).unwrap();
		assert!(programs.pat.is_none());
		assert_eq!(programs.stats().crc_error, 1);

		let mut data = psi_packets(0, &mut cc, &[], &pat_section(&[(1, 0x0100)]));
		data.extend_from_slice(&null);
		programs.decode(&data).unwrap();
		assert!(programs.pat.is_some());
		assert_eq!(programs.stats().crc_error, 1);
	}

	/// How many audio renditions the catalog `path` publishes lists.
	async fn renditions(origin: &moq_net::origin::Producer, path: &str) -> usize {
		let consumer = origin.consume().request(path, None).await.unwrap();
		let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog");
		catalog.audio.renditions.len()
	}

	/// A PAT in two sections starts both programs, each handed the whole table
	/// rather than only the section that completed it.
	#[tokio::test]
	async fn a_pat_in_two_sections_starts_every_program() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin.clone(),
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		programs.decode(&two_section_pat()).unwrap();
		programs.finish().unwrap();
		assert_eq!(programs.programs.len(), 2);
		assert_eq!(renditions(&origin, "event/1.hang").await, 1);
		assert_eq!(renditions(&origin, "event/2.hang").await, 1);
	}

	/// A PAT naming a thousand programs starts only the two whose PMTs the input carries, even
	/// where an undescribed program shares a described one's PMT PID.
	#[tokio::test]
	async fn only_programs_a_pmt_describes_publish() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin.clone(),
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		// Pairs of programs share a PMT PID, so 6 waits on the PID that starts 7.
		let pmt_pid = |program: u16| 0x0100 + program / 2;
		let mut cc = 0;
		let mut data = Vec::new();
		for section in 0..4u8 {
			let entries: Vec<u8> = (1..=250)
				.map(|n| u16::from(section) * 250 + n)
				.flat_map(|n| {
					let pid = pmt_pid(n);
					[(n >> 8) as u8, n as u8, 0xe0 | (pid >> 8) as u8, pid as u8]
				})
				.collect();
			data.extend(psi_packets(
				0,
				&mut cc,
				&[],
				&psi::section(0x00, 1, 0, section, 3, &entries),
			));
		}
		for (program, pid, fills) in [(7u16, 0x0061, [0xAA, 0xBB]), (900, 0x0071, [0xCC, 0xDD])] {
			let pmt = pmt_section(program, 0, &[(MP2, pid, &[])]);
			data.extend(psi_packets(pmt_pid(program), &mut 0, &[], &pmt));
			data.extend(mp2_pes(pid, 0, 90_000, fills));
		}
		programs.decode(&data).unwrap();
		programs.finish().unwrap();

		let started: Vec<_> = programs.programs.iter().map(|program| program.number).collect();
		assert_eq!(started, [7, 900]);
		assert_eq!(renditions(&origin, "event/7.hang").await, 1);
		assert_eq!(renditions(&origin, "event/900.hang").await, 1);
	}

	/// A later PAT moves a waiting program to its new PMT PID and drops one it no longer lists,
	/// so neither the stale PMT nor the dropped program's PMT starts anything.
	#[tokio::test]
	async fn a_later_pat_revises_the_waiting_programs() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin.clone(),
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut cc = 0;
		let mut data = psi_packets(0, &mut cc, &[], &pat_section(&[(1, 0x0100), (2, 0x0101)]));
		let revised = psi::section(0x00, 1, 1, 0, 0, &[0x00, 0x01, 0xe2, 0x00]);
		data.extend(psi_packets(0, &mut cc, &[], &revised));
		for (pid, program, version, es) in [(0x0100, 1, 0, 0x0062), (0x0101, 2, 0, 0x0071), (0x0200, 1, 1, 0x0061)] {
			data.extend(psi_packets(
				pid,
				&mut 0,
				&[],
				&pmt_section(program, version, &[(MP2, es, &[])]),
			));
		}
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		programs.decode(&data).unwrap();
		programs.finish().unwrap();

		let started: Vec<_> = programs.programs.iter().map(|program| program.number).collect();
		assert_eq!(started, [1]);
		assert_eq!(renditions(&origin, "event/1.hang").await, 1);
	}

	/// Of two revisions of a PMT in one packet, the program starts on the later.
	#[tokio::test]
	async fn a_program_starts_on_the_last_pmt_in_its_packet() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin.clone(),
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100)]));
		let mut sections = pmt_section(1, 0, &[(MP2, 0x0062, &[])]);
		sections.extend(pmt_section(1, 1, &[(MP2, 0x0061, &[])]));
		data.extend(psi_packets(0x0100, &mut 0, &[], &sections));
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		programs.decode(&data).unwrap();
		programs.finish().unwrap();
		assert_eq!(renditions(&origin, "event/1.hang").await, 1);
	}

	/// A PMT spanning two packets starts its program, which carries on from the first packet
	/// rather than reading the second alone.
	#[tokio::test]
	async fn a_pmt_spanning_two_packets_starts_its_program() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin.clone(),
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100)]));
		let mut private = vec![0x80, 200];
		private.resize(202, 0);
		let packets = psi_packets(0x0100, &mut 0, &[], &pmt_section(1, 0, &[(MP2, 0x0061, &private)]));
		assert_eq!(packets.len(), 2 * 188, "the PMT spans two packets");
		data.extend(packets);
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		programs.decode(&data).unwrap();
		programs.finish().unwrap();
		assert_eq!(renditions(&origin, "event/1.hang").await, 1);
	}

	/// A corrupt PMT counts once whether it arrives before any program reads its PID, in the
	/// packet that starts one, while one program reads it beside a waiting one, or while both
	/// do, including after a PAT moves a started program onto a waiting one's PID.
	#[test]
	fn a_corrupt_pmt_counts_once_whoever_reads_it() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut null = vec![0x47, 0x1f, 0xff, 0x10];
		null.resize(188, 0xff);
		let pmt = |program| pmt_section(program, 0, &[(MP2, 0x0060 + program, &[])]);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100), (2, 0x0100)]));
		let mut cc = 0;
		let mut starting = corrupt(pmt(1));
		starting.extend(pmt(1));
		for section in [corrupt(pmt(1)), starting, corrupt(pmt(2)), pmt(2), corrupt(pmt(1))] {
			data.extend(psi_packets(0x0100, &mut cc, &[], &section));
		}
		data.extend_from_slice(&null);
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 2);
		assert_eq!(programs.stats().crc_error, 4);

		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut cc = 0;
		let mut data = psi_packets(0, &mut cc, &[], &pat_section(&[(1, 0x0100), (2, 0x0200)]));
		data.extend(psi_packets(0x0100, &mut 0, &[], &pmt(1)));
		let moved = [0x00, 0x01, 0xe2, 0x00, 0x00, 0x02, 0xe2, 0x00];
		data.extend(psi_packets(0, &mut cc, &[], &psi::section(0x00, 1, 1, 0, 0, &moved)));
		data.extend(psi_packets(0x0200, &mut 0, &[], &corrupt(pmt(2))));
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 1);
		assert_eq!(programs.stats().crc_error, 1);

		// The same move landing while the corrupt section is partway in: the program it moved
		// never saw that section begin, so the scan counts it.
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut cc = 0;
		let mut data = psi_packets(0, &mut cc, &[], &pat_section(&[(1, 0x0100), (2, 0x0200)]));
		data.extend(psi_packets(0x0100, &mut 0, &[], &pmt(1)));
		let mut private = vec![0x80, 200];
		private.resize(202, 0);
		let split = psi_packets(
			0x0200,
			&mut 0,
			&[],
			&corrupt(pmt_section(2, 0, &[(MP2, 0x0071, &private)])),
		);
		assert_eq!(split.len(), 2 * 188, "the corrupt PMT spans two packets");
		data.extend_from_slice(&split[..188]);
		data.extend(psi_packets(0, &mut cc, &[], &psi::section(0x00, 1, 1, 0, 0, &moved)));
		data.extend_from_slice(&split[188..]);
		programs.decode(&data).unwrap();
		assert_eq!(programs.stats().crc_error, 1);

		// After that move, a section start abandons the partial for another split corrupt
		// section, which the moved program reads from its start, so it alone counts it.
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut cc = 0;
		let mut data = psi_packets(0, &mut cc, &[], &pat_section(&[(1, 0x0100), (2, 0x0200)]));
		data.extend(psi_packets(0x0100, &mut 0, &[], &pmt(1)));
		data.extend_from_slice(&split[..188]);
		data.extend(psi_packets(0, &mut cc, &[], &psi::section(0x00, 1, 1, 0, 0, &moved)));
		data.extend(psi_packets(
			0x0200,
			&mut 1,
			&[],
			&corrupt(pmt_section(2, 0, &[(MP2, 0x0071, &private)])),
		));
		programs.decode(&data).unwrap();
		assert_eq!(programs.stats().crc_error, 1);

		// A corrupt section begun in the packet that starts a program sharing the PID: the new
		// program reads that packet, so it alone counts the section.
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100), (2, 0x0100)]));
		let mut sections = pmt(1);
		sections.extend(corrupt(pmt_section(2, 0, &[(MP2, 0x0071, &private)])));
		data.extend(psi_packets(0x0100, &mut 0, &[], &sections));
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 1);
		assert_eq!(programs.stats().crc_error, 1);

		// A waiting program's PMT PID that a started program reads only as media: no program
		// reassembles PMTs there, so the scan counts the corrupt section.
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100), (2, 0x0061)]));
		data.extend(psi_packets(0x0100, &mut 0, &[], &pmt(1)));
		data.extend(psi_packets(0x0061, &mut 0, &[], &corrupt(pmt(2))));
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 1);
		assert_eq!(programs.stats().crc_error, 1);
	}

	/// Each program announces as its own publisher instance for the run, so a restart
	/// replaces it rather than resuming into it.
	#[test]
	fn every_program_mints_its_own_epoch() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		programs.decode(&two_section_pat()).unwrap();
		programs.finish().unwrap();
		let epochs: Vec<_> = programs
			.programs
			.iter()
			.map(|program| program.broadcast.route().and_then(|route| route.epoch))
			.collect();
		assert_eq!(epochs.len(), 2);
		assert!(epochs.iter().all(Option::is_some), "every program names its instance");
		assert_ne!(epochs[0], epochs[1]);
	}

	/// A program is announced only once its catalog lists its tracks, never on the PAT alone,
	/// so a relay that queries it the moment it is announced finds them.
	#[test]
	fn a_program_announces_once_it_declares_its_tracks() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let input = two_programs();
		let mut packets = input.chunks(188);
		while programs.programs.is_empty() {
			programs
				.decode(packets.next().expect("a PMT starts a program"))
				.unwrap();
		}
		assert!(
			programs
				.programs
				.iter()
				.all(|program| program.broadcast.route().is_none()),
			"announced on the PSI alone"
		);
		for packet in packets {
			programs.decode(packet).unwrap();
		}
		programs.finish().unwrap();
		assert!(
			programs
				.programs
				.iter()
				.all(|program| program.broadcast.route().is_some()),
			"every program is announced once it has declared its tracks"
		);
	}

	/// Input that ends before any PAT fails rather than exiting cleanly with nothing announced.
	#[test]
	fn input_ending_before_a_pat_fails() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut null = vec![0x47, 0x1f, 0xff, 0x10];
		null.resize(188, 0xff);
		programs.decode(&null).unwrap();
		let Err(err) = programs.finish() else { panic!("no PAT") };
		assert!(err.to_string().contains("PAT"), "{err:#}");
	}

	/// PAT and PMT metadata can name elementary streams before their codec config is known. EOF
	/// there must fail without making an empty program discoverable.
	#[test]
	fn input_ending_after_pmts_but_before_media_fails_unannounced() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let input = two_programs();
		programs.decode(&input[..3 * 188]).unwrap();

		let Err(err) = programs.finish() else {
			panic!("the PMTs have no configured media")
		};
		assert!(err.to_string().contains("declared a track"), "{err:#}");
		assert!(
			programs
				.programs
				.iter()
				.all(|program| program.broadcast.route().is_none()),
			"no empty program is announced"
		);
	}

	/// A partial PAT holds only itself, not the multiplex that follows it.
	#[test]
	fn a_partial_pat_does_not_hold_the_multiplex() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		programs.decode(&fifty_programs()[..188]).unwrap();
		let mut null = vec![0x47, 0x1f, 0xff, 0x10];
		null.resize(188, 0xff);
		for _ in 0..1000 {
			programs.decode(&null).unwrap();
		}
		assert!(programs.programs.is_empty());
		assert!(programs.demux.held() < 188, "held {} bytes", programs.demux.held());
	}

	/// A corrupt PAT section sharing a packet with the good one that starts the programs is
	/// counted once.
	#[test]
	fn a_corrupt_pat_beside_a_good_one_counts_once() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let mut sections = corrupt(pat_section(&[(1, 0x0100)]));
		sections.extend(pat_section(&[(1, 0x0100)]));
		let mut data = psi_packets(0, &mut 0, &[], &sections);
		let mut null = vec![0x47, 0x1f, 0xff, 0x10];
		null.resize(188, 0xff);
		data.extend_from_slice(&null);
		programs.decode(&data).unwrap();
		assert!(programs.pat.is_some());
		assert_eq!(programs.stats().crc_error, 1);
	}

	/// A second program claiming an elementary PID another already reads keeps the PID's
	/// continuity chain, so a counter gap right after its PMT still counts.
	#[test]
	fn a_shared_pid_keeps_its_chain_when_another_program_claims_it() {
		const MP2: u8 = 0x03;
		let es = |cc: u8| {
			let mut pkt = vec![0x47, 0x00, 0x61, 0x10 | cc];
			pkt.resize(188, 0xff);
			pkt
		};
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);
		let pmt = |program: u16| {
			psi_packets(
				program << 8,
				&mut 0,
				&[],
				&pmt_section(program, 0, &[(MP2, 0x0061, &[])]),
			)
		};
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100), (2, 0x0200)]));
		data.extend(pmt(1));
		data.extend(es(0));
		data.extend(pmt(2));
		data.extend(es(2));
		programs.decode(&data).unwrap();
		assert_eq!(programs.stats().continuity_count_error, 1);
	}

	/// The input is held until the PAT is whole, then each program publishes as its own
	/// broadcast carrying only that program, its catalog clock anchored on its own first frame
	/// apart on one shared clock.
	#[tokio::test]
	async fn every_program_publishes_its_own_broadcast() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin.clone(),
			moq_net::Path::from_static("event.hang"),
			catalog::Config::default(),
		);

		let input = two_programs();
		let before = kio::clock::wall();
		programs.decode(&input[..100]).unwrap();
		assert!(
			programs.programs.is_empty(),
			"no program is known before the PAT is whole"
		);
		programs.decode(&input[100..]).unwrap();
		programs.finish().unwrap();
		let after = kio::clock::wall();

		for (path, fills) in [("event/1.hang", [0xAA, 0xBB]), ("event/2.hang", [0xCC, 0xDD])] {
			let consumer = origin.consume().request(path, None).await.unwrap();
			let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
				.await
				.unwrap()
				.next()
				.await
				.unwrap()
				.expect("a catalog");
			let renditions: Vec<_> = catalog.audio.renditions.iter().collect();
			assert_eq!(renditions.len(), 1, "{path} carries its own program's one stream");
			let (name, config) = renditions[0];
			let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
			let container = Container::try_from(config).unwrap();
			let frame = Consumer::new(track, container)
				.read_frame()
				.await
				.unwrap()
				.expect("a frame");
			assert!(fills.contains(&frame.payload[4]), "{path} carries only its own program");
			let skew = Duration::from_secs(2);
			let wall = catalog
				.clock
				.expect("the catalog advertises a clock")
				.wall_clock(frame.timestamp)
				.unwrap();
			assert!(
				before - skew <= wall && wall <= after + skew,
				"{path} is live on arrival: {wall:?} not in {before:?}..={after:?}"
			);
		}
	}
}
