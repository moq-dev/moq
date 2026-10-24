//! Every program of a multiplex, each imported as its own broadcast.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Context;
use mpeg2ts::ts::{Pid, TsPacket};

use super::health::Health;
use super::import::{Framer, PatReader, SectionReassembler};
use super::{Ext, Import, psi, stats};
use crate::catalog;

/// Imports every program the first PAT lists, each as its own broadcast on `origin`, with its
/// own clock and catalog.
///
/// Program `n` of `event.hang` publishes on `event/n.hang`, keeping the catalog suffix last so
/// format detection still sees it; a name without one gains a plain `/n`. A program starts once
/// its PMT lists an elementary stream, so one the PAT names but the input never describes costs
/// nothing. Input ahead of that PMT is dropped, as a single [`Import`] drops media ahead of its
/// PSI. A later PAT moves a waiting program to the PMT PID it names and drops one it no longer
/// lists. A program a later PAT adds is not published; removing a started one ends the import.
pub struct Programs {
	origin: moq_net::origin::Producer,
	name: moq_net::PathOwned,
	config: catalog::Config,
	/// Input not yet read for the PAT and the waiting PMTs, a trailing partial packet at most.
	/// The started programs already hold its first `fed` bytes.
	pending: Vec<u8>,
	fed: usize,
	framer: Framer,
	/// Reads PATs while a program waits.
	pat_reader: PatReader,
	/// The latest whole PAT, from the first that lists a program, which seeds every importer.
	pat: Option<psi::Pat>,
	/// The programs the first PAT lists and no PMT has started yet, by the PMT PID the latest
	/// PAT names for them.
	waiting: HashMap<Pid, Waiting>,
	/// PSI sections dropped for a bad CRC while no importer read their PID, keyed by PID.
	crc_errors: BTreeMap<u16, u64>,
	/// Grades the input until the first program starts, whose importer carries it on.
	health: Option<Health>,
	/// Empty until a PMT starts a program.
	programs: Vec<Program>,
}

/// The programs waiting for their PMT on one PID.
#[derive(Default)]
struct Waiting {
	sections: SectionReassembler,
	programs: HashSet<u16>,
}

/// One program's broadcast.
struct Program {
	/// The PAT's program number.
	number: u16,
	/// Keeps the origin-created broadcast alive; the importer's clone doesn't.
	broadcast: moq_net::broadcast::Producer,
	import: Import<Ext>,
	catalog: catalog::Producer<Ext>,
	/// The route and catalog track until the catalog first names a servable track: announced any
	/// sooner, a relay would ask for tracks it cannot serve yet.
	pending: Option<(moq_net::origin::Route, moq_net::track::Consumer)>,
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
			pending: Vec::new(),
			fed: 0,
			framer: Framer::default(),
			pat_reader: PatReader::default(),
			pat: None,
			waiting: HashMap::new(),
			crc_errors: BTreeMap::new(),
			health: Some(Health::default()),
			programs: Vec::new(),
		}
	}

	/// Demux a chunk of the multiplex. A trailing partial packet is retained for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		if !self.programs.is_empty() && self.waiting.is_empty() {
			// Every program has started, and holds the partial packet `pending` kept.
			self.pending.clear();
			return feed(&mut self.programs, data);
		}
		self.pending.extend_from_slice(data);
		let mut off = 0;
		while let Some(at) = self.framer.next(&self.pending, &mut off) {
			let pkt: [u8; TsPacket::SIZE] = self.pending[at..at + TsPacket::SIZE].try_into().unwrap();
			if let Some(health) = &mut self.health {
				health.routed(&self.pending, at);
				// No program clock runs before a PMT.
				health.packet(&pkt, None);
			}
			let described = self.psi(&pkt);
			if described.is_empty() {
				continue;
			}
			// The programs already running read through this packet; the new ones start after it.
			feed(&mut self.programs, &self.pending[self.fed..off])?;
			self.fed = off;
			for pmt in described {
				// The first importer grades on from here, as one reading from the start would.
				// Later ones count a subset of its errors, which `stats` merges.
				let health = match self.health.take() {
					Some(mut health) => {
						health.drained(&self.pending[..off], off);
						health
					}
					None => Health::default(),
				};
				let program = self.start(pmt, health)?;
				self.programs.push(program);
			}
		}
		if let Some(health) = &mut self.health {
			health.drained(&self.pending, off);
		}
		feed(&mut self.programs, &self.pending[self.fed..])?;
		self.pending.drain(..off);
		self.fed = self.pending.len();
		Ok(())
	}

	/// Read one packet for the PAT or a waiting PMT, returning the PMTs of the programs it starts.
	fn psi(&mut self, pkt: &[u8; TsPacket::SIZE]) -> Vec<psi::Pmt> {
		let pid = (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]);
		if pid == Pid::PAT {
			// Once a program starts, its importer counts the PAT's errors.
			let mut counted = 0;
			let crc_error = if self.programs.is_empty() {
				self.crc_errors.entry(Pid::PAT).or_default()
			} else {
				&mut counted
			};
			if let Some(pat) = self.pat_reader.push(pkt, crc_error) {
				self.wait(pat);
			}
			return Vec::new();
		}
		let Some((pid, waiting)) = Pid::new(pid)
			.ok()
			.and_then(|pid| Some((pid, self.waiting.get_mut(&pid)?)))
		else {
			return Vec::new();
		};
		let mut sections = Vec::new();
		waiting.sections.push(pkt, &mut sections);
		// Decided before this packet starts anyone: the new importers miss its sections.
		let read = self.programs.iter().any(|program| program.import.reads_pmt(pid));
		let mut latest: Vec<psi::Pmt> = Vec::new();
		for section in sections {
			// Other tables may share the PID.
			if section.first() != Some(&psi::PMT_TABLE_ID) {
				continue;
			}
			if !psi::crc_ok(&section) {
				if !read {
					*self.crc_errors.entry(pid.as_u16()).or_default() += 1;
					tracing::warn!(pid = pid.as_u16(), "dropped a PSI section with a bad CRC");
				}
				continue;
			}
			// A revision later in the packet supersedes an earlier one, since the importer
			// starts after it.
			if let Some(pmt) = psi::Pmt::parse(&section)
				&& waiting.programs.contains(&pmt.program_number)
			{
				latest.retain(|seen| seen.program_number != pmt.program_number);
				latest.push(pmt);
			}
		}
		latest.retain(|pmt| !pmt.streams.is_empty());
		for pmt in &latest {
			waiting.programs.remove(&pmt.program_number);
		}
		if waiting.programs.is_empty() {
			self.waiting.remove(&pid);
		}
		latest
	}

	/// Wait for the PMT of each program the first whole PAT listing one names. A later PAT moves
	/// a waiting program to the PMT PID it names, and drops one it no longer lists.
	fn wait(&mut self, pat: psi::Pat) {
		let mut listed = HashMap::new();
		for entry in &pat.programs {
			// An importer takes a program's first entry, so a repeated number waits there alone.
			if entry.program_number != 0 {
				listed.entry(entry.program_number).or_insert(entry.pmt_pid);
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
		self.pat = Some(pat);
	}

	/// Create the broadcast of the program `pmt` describes, its importer seeded with the PAT and
	/// that PMT rather than replaying their packets, so nothing counted here is counted again.
	fn start(&self, pmt: psi::Pmt, health: Health) -> anyhow::Result<Program> {
		let pat = self.pat.as_ref().expect("a PMT waits on its PAT");
		let number = pmt.program_number;
		let name = program_broadcast(self.name.as_str(), number);
		let mut broadcast = self
			.origin
			.create_broadcast(&name)
			.with_context(|| format!("failed to create broadcast {name}"))?;
		// TS carries undecoded elementary streams verbatim, so each catalog carries the
		// `mpegts` extension, and owns its broadcast's catalog tracks.
		let config = self
			.config
			.clone()
			.with_catalog(catalog::hang::Catalog::<Ext>::default());
		let catalog = catalog::Producer::new(&mut broadcast, config)?;
		let mut import = Import::new(broadcast.clone(), catalog.reserve())
			.with_program(number)
			.resume(health);
		import.handle_pat(pat)?;
		import.handle_pmt(pmt)?;
		// Each program is its own publisher instance for this run, so a restart
		// replaces it rather than resuming into it.
		let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
		let listed = broadcast.consume().track(hang::Catalog::DEFAULT_NAME)?;
		Ok(Program {
			number,
			broadcast,
			import,
			catalog,
			pending: Some((route, listed)),
		})
	}

	/// Every program's counters in one map: PIDs are unique across a multiplex.
	///
	/// Every importer reads the PAT, and programs sharing a PMT PID read the same PMT sections,
	/// each from when it started. The earliest importer read whatever a later one did, so a
	/// dropped section counts once however many read it, and so does each TR 101 290 error.
	/// Sections dropped before any importer read their PID are counted here, and so is the
	/// grading of the input before the first program started.
	pub fn stats(&self) -> stats::Snapshot {
		let mut stats = stats::Snapshot::default();
		let mut crc_errors = BTreeMap::<u16, u64>::new();
		let mut errors = super::health::Errors::default();
		if let Some(health) = &self.health {
			errors.max(health.errors());
		}
		for program in &self.programs {
			stats.streams.extend(program.import.stats().streams);
			for (&pid, &count) in program.import.crc_errors() {
				let merged = crc_errors.entry(pid).or_default();
				*merged = (*merged).max(count);
			}
			errors.max(program.import.errors());
		}
		stats.crc_error = crc_errors.values().chain(self.crc_errors.values()).sum();
		errors.report(&mut stats);
		stats
	}

	/// Finish each program's tracks, then its catalog while it still lists them.
	///
	/// Returns each program's [`Finish`](moq_net::broadcast::Finish), which keeps it servable
	/// until its media is stale; dropping one closes that program.
	/// Fails when the input ended before its PAT listed a program or any program declared a track.
	pub fn finish(&mut self) -> anyhow::Result<Vec<moq_net::broadcast::Finish>> {
		anyhow::ensure!(self.pat.is_some(), "input ended before its PAT listed a program");
		let mut announced = false;
		let mut finished = Vec::with_capacity(self.programs.len());
		for program in &mut self.programs {
			program.import.finish()?;
			announced |= program.announce_if_listed()?;
			program.catalog.finish()?;
			finished.push(program.broadcast.clone().finish());
		}
		anyhow::ensure!(announced, "input ended before any program declared a track");
		Ok(finished)
	}

	/// Abort every program's catalog and media tracks with `err`. Consumes the importer.
	pub fn abort(self, err: moq_net::Error) {
		for program in self.programs {
			program.catalog.abort(err.clone());
			program.import.abort(err.clone());
		}
	}
}

/// Hand `data` to every program, announcing each whose catalog has gone out.
fn feed(programs: &mut [Program], data: &[u8]) -> anyhow::Result<()> {
	for program in programs {
		program.import.decode(data)?;
		program.announce_if_listed()?;
	}
	Ok(())
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

	/// The programs of the PAT a whole buffer starts waiting on.
	fn pat_programs(data: &[u8]) -> Option<Vec<u16>> {
		let mut programs = Programs::new(
			crate::source::produce_origin(),
			"event.hang",
			catalog::Config::default(),
		);
		programs.decode(data).unwrap();
		Some(
			programs
				.pat?
				.programs
				.iter()
				.map(|entry| entry.program_number)
				.collect(),
		)
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

	/// A PAT too big for one packet is read whole, and its importer is handed all of it, so
	/// the program listed in its second packet publishes.
	#[tokio::test]
	async fn a_pat_spanning_two_packets_is_read_whole() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
		let input = fifty_programs();
		for chunk in input.chunks(100) {
			programs.decode(chunk).unwrap();
		}
		let _finished = programs.finish().unwrap();
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

	/// A corrupt PMT counts once whether it arrives before any importer reads its PID, while
	/// one importer reads it beside a waiting program, or while both of them do.
	#[test]
	fn a_corrupt_pmt_counts_once_whoever_reads_it() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
		let mut null = vec![0x47, 0x1f, 0xff, 0x10];
		null.resize(188, 0xff);
		let pmt = |program| pmt_section(program, 0, &[(MP2, 0x0060 + program, &[])]);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100), (2, 0x0100)]));
		let mut cc = 0;
		for section in [corrupt(pmt(1)), pmt(1), corrupt(pmt(2)), pmt(2), corrupt(pmt(1))] {
			data.extend(psi_packets(0x0100, &mut cc, &[], &section));
		}
		data.extend_from_slice(&null);
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 2);
		assert_eq!(programs.stats().crc_error, 3);
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

	/// A PAT in two sections starts both programs, each importer seeded with the whole table
	/// rather than only the section that completed it.
	#[tokio::test]
	async fn a_pat_in_two_sections_starts_every_program() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
		programs.decode(&two_section_pat()).unwrap();
		let _finished = programs.finish().unwrap();
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
		let _finished = programs.finish().unwrap();

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
		data.extend(psi_packets(
			0x0100,
			&mut 0,
			&[],
			&pmt_section(1, 0, &[(MP2, 0x0062, &[])]),
		));
		data.extend(psi_packets(
			0x0101,
			&mut 0,
			&[],
			&pmt_section(2, 0, &[(MP2, 0x0071, &[])]),
		));
		data.extend(psi_packets(
			0x0200,
			&mut 0,
			&[],
			&pmt_section(1, 1, &[(MP2, 0x0061, &[])]),
		));
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		programs.decode(&data).unwrap();
		let _finished = programs.finish().unwrap();

		let started: Vec<_> = programs.programs.iter().map(|program| program.number).collect();
		assert_eq!(started, [1]);
		assert_eq!(renditions(&origin, "event/1.hang").await, 1);
	}

	/// Of two revisions of a PMT in one packet, the program starts on the later, as its importer
	/// only reads the packets after it.
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
		let _finished = programs.finish().unwrap();
		assert_eq!(renditions(&origin, "event/1.hang").await, 1);
	}

	/// A transport error ahead of every PMT counts once: the scan grades it, and the first
	/// importer carries that grading on rather than starting blind.
	#[test]
	fn a_transport_error_before_the_pmts_counts_once() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100), (2, 0x0200)]));
		let mut flagged = vec![0x47, 0x9f, 0xff, 0x10];
		flagged.resize(188, 0xff);
		data.extend_from_slice(&flagged);
		programs.decode(&data).unwrap();
		assert!(programs.programs.is_empty());
		assert_eq!(programs.stats().transport_error, 1);

		let mut data = Vec::new();
		for (program, pid, fills) in [(1u16, 0x0061, [0xAA, 0xBB]), (2, 0x0071, [0xCC, 0xDD])] {
			let pmt = pmt_section(program, 0, &[(MP2, pid, &[])]);
			data.extend(psi_packets(program << 8, &mut 0, &[], &pmt));
			data.extend(mp2_pes(pid, 0, 90_000, fills));
		}
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 2);
		assert_eq!(programs.stats().transport_error, 1);
	}

	/// Each program announces as its own publisher instance for the run, so a restart
	/// replaces it rather than resuming into it.
	#[test]
	fn every_program_mints_its_own_epoch() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
		programs.decode(&two_section_pat()).unwrap();
		let _finished = programs.finish().unwrap();
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
				.decode(packets.next().expect("the PAT starts the programs"))
				.unwrap();
		}
		assert!(
			programs
				.programs
				.iter()
				.all(|program| program.broadcast.route().is_none()),
			"announced on the PAT alone"
		);
		for packet in packets {
			programs.decode(packet).unwrap();
		}
		let _finished = programs.finish().unwrap();
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
		assert!(
			programs.pending.len() < 2 * 188,
			"held {} bytes",
			programs.pending.len()
		);
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

	/// The input is held until the PAT is whole, then each program publishes as its own
	/// broadcast carrying only that program, its catalog clock anchored on its own first frame
	/// apart on one shared clock.
	#[tokio::test]
	async fn every_program_publishes_its_own_broadcast() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());

		let input = two_programs();
		let before = std::time::SystemTime::now();
		programs.decode(&input[..100]).unwrap();
		assert!(
			programs.programs.is_empty(),
			"no program is known before the PAT is whole"
		);
		programs.decode(&input[100..]).unwrap();
		let _finished = programs.finish().unwrap();
		let after = std::time::SystemTime::now();

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
