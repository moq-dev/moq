//! Every program of a multiplex, each imported as its own broadcast.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Context;
use mpeg2ts::ts::Pid;

use super::demux::{Demux, Packet};
use super::import::SectionReassembler;
use super::{Ext, Import, catalog::SI_PIDS, psi, stats};
use crate::catalog;

/// Imports every program the first PAT lists, each as its own broadcast on `origin`, with its
/// own clock and catalog.
///
/// Program `n` of `event.hang` publishes on `event/n.hang`, keeping the catalog suffix last so
/// format detection still sees it; a name without one gains a plain `/n`. A program starts once
/// its PMT lists an elementary stream, so one the PAT names but the input never describes costs
/// nothing. Input ahead of that PMT is dropped, as a single [`Import`] drops media ahead of its
/// PSI; its SI arrives with the next repetition, as for an import joining mid-stream. A later PAT moves a waiting program to the PMT PID it names and drops one it no longer
/// lists. A program a later PAT adds is not published; removing a started one ends the import.
///
/// The multiplex is parsed once: framing, continuity, the PAT and the SI sections are shared,
/// and each program reads only the packets on the PIDs its PAT entry and PMT claim.
pub struct Programs {
	origin: moq_net::origin::Producer,
	name: moq_net::PathOwned,
	config: catalog::Config,
	demux: Demux,
	/// The latest whole PAT, from the first that lists a program, which seeds every program.
	pat: Option<psi::Pat>,
	/// The programs the first PAT lists and no PMT has started yet, by the PMT PID the latest
	/// PAT names for them.
	waiting: HashMap<u16, Waiting>,
	/// PMT sections dropped for a bad CRC on a PID no program read, by PID.
	crc_errors: BTreeMap<u16, u64>,
	/// Empty until a PMT starts a program.
	programs: Vec<Program>,
	/// The programs reading each PID they claim, indexed by PID, in program order. The PAT and
	/// the SI PIDs go to every program instead.
	readers: Vec<Vec<usize>>,
	/// The programs the current [`decode`](Self::decode) call routed a packet to.
	touched: Vec<usize>,
}

/// The programs waiting for their PMT on one PID.
#[derive(Default)]
struct Waiting {
	sections: SectionReassembler,
	programs: HashSet<u16>,
	/// Whether a program read this PID when the section still assembling began, and so counts
	/// it if it is corrupt.
	began_read: bool,
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

impl Programs {
	/// Split the multiplex into broadcasts named after `name`, each catalog built from `config`.
	pub fn new(origin: moq_net::origin::Producer, name: impl moq_net::AsPath, config: catalog::Config) -> Self {
		Self {
			origin,
			name: name.as_path().to_owned(),
			config,
			demux: Demux::new(true),
			pat: None,
			waiting: HashMap::new(),
			crc_errors: BTreeMap::new(),
			programs: Vec::new(),
			readers: vec![Vec::new(); 1 << 13],
			touched: Vec::new(),
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
				self.wait(pat);
			} else if !self.waiting.is_empty() {
				self.describe(&packet)?;
			}
			// Every program reads the PAT, and the SI sections the parser reassembled.
			let everyone = packet.pid == Pid::PAT || (SI_PIDS.contains(&packet.pid) && !packet.sections.is_empty());
			let readers = match everyone {
				true => None,
				false => match self.readers[usize::from(packet.pid)].as_slice() {
					[] => continue,
					readers => Some(readers),
				},
			};
			let mut changed = Vec::new();
			for at in 0..readers.map_or(self.programs.len(), <[usize]>::len) {
				let index = readers.map_or(at, |readers| readers[at]);
				let program = &mut self.programs[index];
				if !std::mem::replace(&mut program.touched, true) {
					self.touched.push(index);
				}
				let import = &mut program.import;
				import.packet(&packet)?;
				if import.claims_changed() {
					changed.push(index);
				}
			}
			drop(packet);
			// Only a PAT or PMT moves claims or restarts a route.
			for index in changed {
				for pid in self.programs[index].import.restarts() {
					// Another program still reading the PID judges its partial bytes against
					// this chain, so only the PID's sole reader restarts it.
					if self.readers[usize::from(pid)].iter().all(|&reader| reader == index) {
						demux.restart(pid);
					}
				}
				self.reclaim(index);
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

	/// Wait for the PMT of each program the first whole PAT listing one names. A later PAT moves
	/// a waiting program to the PMT PID it names, and drops one it no longer lists.
	fn wait(&mut self, pat: &psi::Pat) {
		// The PAT repeats several times a second, mostly unchanged.
		if self.pat.as_ref() == Some(pat) {
			return;
		}
		let mut listed = HashMap::new();
		for entry in &pat.programs {
			// An importer takes a program's first entry, so a repeated number waits there alone.
			if entry.program_number != 0 {
				listed.entry(entry.program_number).or_insert(entry.pmt_pid.as_u16());
			}
		}
		if self.pat.is_none() {
			if listed.is_empty() {
				return;
			}
			for (&number, &pid) in &listed {
				self.waiting.entry(pid).or_default().programs.insert(number);
			}
		} else {
			let mut moved = Vec::new();
			self.waiting.retain(|pid, waiting| {
				waiting.programs.retain(|number| match listed.get(number) {
					Some(listed) if listed == pid => true,
					Some(&listed) => {
						moved.push((*number, listed));
						false
					}
					None => false,
				});
				!waiting.programs.is_empty()
			});
			for (number, pid) in moved {
				self.waiting.entry(pid).or_default().programs.insert(number);
			}
		}
		self.pat = Some(pat.clone());
	}

	/// Read a packet on a waiting PMT PID, and start each program whose PMT it completes with a
	/// stream listed.
	///
	/// A started program reads this packet itself, from the reassembler as it stood before it,
	/// so its own PMT path applies the PMT, counts the packet's corrupt sections, and starts its
	/// clock from the PCR already seen. Of several revisions in the packet, the last decides.
	fn describe(&mut self, packet: &Packet) -> anyhow::Result<()> {
		let Some(waiting) = self.waiting.get_mut(&packet.pid) else {
			return Ok(());
		};
		let before = waiting.sections.clone();
		let read = self.readers[usize::from(packet.pid)]
			.iter()
			.any(|&index| self.programs[index].import.reads_pmt(packet.pid));
		let began = waiting.began_read;
		let mut sections = Vec::new();
		waiting.sections.push(packet.pkt, &mut sections);
		let resumed = waiting.sections.resumed();
		if waiting.sections.fresh() {
			waiting.began_read = read;
		}
		// Corrupt sections no program read from their start.
		let mut corrupt = 0;
		let mut latest: Vec<psi::Pmt> = Vec::new();
		for (index, section) in sections.into_iter().enumerate() {
			// Other tables may share the PID.
			if section.first() != Some(&psi::PMT_TABLE_ID) {
				continue;
			}
			if !psi::crc_ok(&section) {
				let read = if index == 0 && resumed { began } else { read };
				corrupt += u64::from(!read);
				continue;
			}
			if let Some(pmt) = psi::Pmt::parse(&section)
				&& waiting.programs.contains(&pmt.program_number)
			{
				latest.retain(|seen| seen.program_number != pmt.program_number);
				latest.push(pmt);
			}
		}
		latest.retain(|pmt| !pmt.streams.is_empty());
		if latest.is_empty() {
			if corrupt > 0 {
				*self.crc_errors.entry(packet.pid).or_default() += corrupt;
				tracing::warn!(pid = packet.pid, "dropped a PSI section with a bad CRC");
			}
			return Ok(());
		}
		for pmt in &latest {
			waiting.programs.remove(&pmt.program_number);
		}
		if waiting.programs.is_empty() {
			self.waiting.remove(&packet.pid);
		}
		for pmt in latest {
			self.start(pmt.program_number, packet.pid, before.clone())?;
		}
		// The programs just started read this packet, so they read any section it began.
		if let Some(waiting) = self.waiting.get_mut(&packet.pid) {
			waiting.began_read = true;
		}
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
			touched: false,
		});
		self.reclaim(self.programs.len() - 1);
		Ok(())
	}

	/// Move program `index` in [`Self::readers`] to the PIDs it claims now.
	fn reclaim(&mut self, index: usize) {
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
		program.claims = claims;
	}

	/// Every program's counters in one map: PIDs are unique across a multiplex.
	///
	/// The multiplex is graded once. Programs sharing a PMT PID read the same PMT sections, so
	/// a dropped section counts once however many read it, and programs sharing a PCR PID
	/// count its faults once too. PMT sections dropped before any program read their PID are
	/// counted here.
	pub fn stats(&self) -> stats::Snapshot {
		let mut stats = stats::Snapshot::default();
		let mut crc_errors = BTreeMap::<u16, u64>::new();
		let mut errors = self.demux.errors().clone();
		for program in &self.programs {
			stats.streams.extend(program.import.stats().streams);
			for (&pid, &count) in program.import.crc_errors() {
				let merged = crc_errors.entry(pid).or_default();
				*merged = (*merged).max(count);
			}
			errors.max(program.import.errors());
		}
		stats.crc_error = crc_errors.values().chain(self.crc_errors.values()).sum::<u64>() + self.demux.pat_crc_error();
		errors.report(&mut stats);
		stats
	}

	/// Finish each program's tracks, then its catalog while it still lists them.
	///
	/// Returns each program's broadcast. The finished tracks stay servable while this importer
	/// is held, so hand both to a [`Linger`](crate::Linger) to keep them up for late
	/// subscribers. Fails when the input ended before its PAT listed a program or any program
	/// declared a track.
	pub fn finish(&mut self) -> anyhow::Result<Vec<moq_net::broadcast::Producer>> {
		self.demux.finish();
		anyhow::ensure!(self.pat.is_some(), "input ended before its PAT listed a program");
		let mut announced = false;
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
	match catalog::CatalogFormat::detect(name) {
		Some(format) => {
			let stem = &name[..name.len() - format.extension().len()];
			format!("{stem}/{program}{}", format.extension())
		}
		None => format!("{name}/{program}"),
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;
	use crate::catalog::hang::Container;
	use crate::container::Consumer;
	use crate::container::ts::import::test::{
		MP2, corrupt, fifty_programs, mp2_pes, pat_section, pmt_section, psi_packets, two_programs, two_section_pat,
	};

	#[tokio::test(start_paused = true)]
	async fn internal_program_abort_preserves_the_catalog_error() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
		let input = fifty_programs();
		for chunk in input.chunks(100) {
			programs.decode(chunk).unwrap();
		}
		programs.finish().unwrap();
		assert_eq!(programs.programs.len(), 1, "only program 50 has a PMT");

		let consumer = origin.consume().request_broadcast("event/50.hang", None).await.unwrap();
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let consumer = origin.consume().request_broadcast(path, None).await.unwrap();
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
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
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
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());

		let input = two_programs();
		let before = moq_clock::wall();
		programs.decode(&input[..100]).unwrap();
		assert!(
			programs.programs.is_empty(),
			"no program is known before the PAT is whole"
		);
		programs.decode(&input[100..]).unwrap();
		programs.finish().unwrap();
		let after = moq_clock::wall();

		for (path, fills) in [("event/1.hang", [0xAA, 0xBB]), ("event/2.hang", [0xCC, 0xDD])] {
			let consumer = origin.consume().request_broadcast(path, None).await.unwrap();
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
