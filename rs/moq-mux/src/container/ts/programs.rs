//! Every program of a multiplex, each imported as its own broadcast.

use std::collections::BTreeMap;

use anyhow::Context;
use mpeg2ts::ts::{Pid, TsPacket};

use super::import::{Framer, PatReader};
use super::{Ext, Import, psi, stats};
use crate::catalog;

/// Imports every program the PAT lists, each as its own broadcast on `origin`, with its own
/// clock and catalog.
///
/// Program `n` of `event.hang` publishes on `event/n.hang`, keeping the catalog suffix last so
/// format detection still sees it; a name without one gains a plain `/n`. Input ahead of the
/// first PAT listing a program is dropped, as a single [`Import`] drops media ahead of its PSI.
///
/// The PAT is followed as it changes. A program a later PAT adds publishes as the first
/// PAT's do, from the packet after that PAT. One it removes is retired: its tracks finish and
/// its broadcast is withdrawn, while the other programs carry on. A program that returns
/// publishes a new instance, under a new epoch. A program whose PMT never arrives waits,
/// unannounced: it publishes nothing until its PMT declares a track.
pub struct Programs {
	origin: moq_net::origin::Producer,
	name: moq_net::PathOwned,
	config: catalog::Config,
	/// Input not yet framed: before the first PAT, all of it; after, a trailing partial packet
	/// the programs already have.
	pending: Vec<u8>,
	/// Reads every PAT out of the input.
	scan: PatScan,
	/// Empty until the PAT arrives, and again once a PAT removes every program.
	programs: Vec<Program>,
	/// Whether a PAT has listed a program yet.
	listed: bool,
	/// Whether any program has announced, retired ones included.
	announced: bool,
	/// What retired programs counted, so the multiplex's counters keep it.
	retired: Retired,
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
	/// Where in [`Programs::pending`] this program's input starts, for the one call that adds
	/// it: just after the PAT that did.
	start: usize,
}

/// The counters of programs a later PAT removed.
#[derive(Default)]
struct Retired {
	/// Each PID's row as its last program left it, until a program carrying it again replaces it.
	streams: BTreeMap<u16, stats::Stream>,
	crc_errors: BTreeMap<u16, u64>,
	errors: super::health::Errors,
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
			scan: PatScan::default(),
			programs: Vec::new(),
			listed: false,
			announced: false,
			retired: Retired::default(),
		}
	}

	/// Demux a chunk of the multiplex. A trailing partial packet is retained for the next call.
	///
	/// The input is cut at each PAT that changes the programs: what comes before it reaches
	/// the programs the PAT before it listed, so a removed program never reads the PAT that
	/// removed it, and an added one starts just after it.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		// What the programs already have: the partial packet the last call kept.
		let mut fed = if self.programs.is_empty() {
			0
		} else {
			self.pending.len()
		};
		self.pending.extend_from_slice(data);
		let mut off = 0;
		while let Some(at) = self.scan.framer.next(&self.pending, &mut off) {
			let pkt: &[u8; TsPacket::SIZE] = self.pending[at..at + TsPacket::SIZE].try_into().unwrap();
			if (u16::from(pkt[1] & 0x1f) << 8 | u16::from(pkt[2])) != Pid::PAT {
				continue;
			}
			// The importers count what they read themselves.
			let mut read = 0;
			let crc_error = match self.programs.is_empty() {
				true => &mut self.scan.crc_error,
				false => &mut read,
			};
			let Some(pat) = self.scan.pat.push(pkt, crc_error) else {
				continue;
			};
			let numbers = pat.program_numbers();
			let current = |number: &u16| self.programs.iter().any(|program| program.number == *number);
			if numbers.iter().all(current) && self.programs.iter().all(|program| numbers.contains(&program.number)) {
				continue;
			}
			let cut = at.max(fed);
			self.feed(fed, cut)?;
			fed = cut;
			self.follow(&pat, at + TsPacket::SIZE)?;
		}
		self.feed(fed, self.pending.len())?;
		for program in &mut self.programs {
			program.start = 0;
		}
		// Before the first PAT, what precedes the partial packet is dropped; after it, the
		// programs have it all.
		self.pending.drain(..off);
		Ok(())
	}

	/// Hand `pending[from..to]` to every program, each from its own start.
	fn feed(&mut self, from: usize, to: usize) -> anyhow::Result<()> {
		for program in &mut self.programs {
			let from = from.max(program.start);
			if from < to {
				program.import.decode(&self.pending[from..to])?;
			}
			self.announced |= program.announce_if_listed()?;
		}
		Ok(())
	}

	/// Bring the programs in line with `pat`: retire those it no longer lists, keeping the
	/// order of the rest, and start those it adds on the input from `start`.
	fn follow(&mut self, pat: &psi::Pat, start: usize) -> anyhow::Result<()> {
		let numbers = pat.program_numbers();
		let (kept, gone) = std::mem::take(&mut self.programs)
			.into_iter()
			.partition(|program| numbers.contains(&program.number));
		self.programs = kept;
		for program in gone {
			self.retire(program);
		}
		for number in numbers {
			if self.programs.iter().any(|program| program.number == number) {
				continue;
			}
			if self.listed {
				tracing::info!(program = number, "TS program joined the PAT");
			}
			self.start(number, pat, start)?;
		}
		self.listed |= !self.programs.is_empty();
		Ok(())
	}

	/// Publish program `number` as its own broadcast, its importer seeded with `pat`.
	fn start(&mut self, number: u16, pat: &psi::Pat, start: usize) -> anyhow::Result<()> {
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
		let mut import = Import::new(broadcast.clone(), catalog.reserve()).with_program(number);
		// Seeded with the PAT rather than replaying its packets, so every section reaches the
		// importer and nothing already counted is counted again.
		import.handle_pat(pat)?;
		// Each program, and each return of one, is its own publisher instance, so a restart
		// replaces it rather than resuming into it.
		let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
		let listed = broadcast.consume().track(hang::Catalog::DEFAULT_NAME)?;
		self.programs.push(Program {
			number,
			broadcast,
			import,
			catalog,
			pending: Some((route, listed)),
			start,
		});
		Ok(())
	}

	/// End a program a PAT no longer lists: finish its tracks, then its catalog, and close its
	/// broadcast, so it is withdrawn now rather than lingering. Subscribers already reading a
	/// track read it to its end. The rest of the multiplex carries on whatever this program
	/// left unfinished.
	fn retire(&mut self, mut program: Program) {
		tracing::info!(
			program = program.number,
			"TS program left the PAT; retiring its broadcast"
		);
		let finished = program
			.import
			.finish()
			.and_then(|()| program.catalog.finish().map_err(anyhow::Error::from));
		if let Err(err) = finished {
			tracing::warn!(program = program.number, error = %err, "TS program retired unfinished");
		}
		program.broadcast.close();
		self.retired.streams.extend(program.import.stats().streams);
		for (&pid, &count) in program.import.crc_errors() {
			let merged = self.retired.crc_errors.entry(pid).or_default();
			*merged = (*merged).max(count);
		}
		self.retired.errors.max(program.import.errors());
	}

	/// Every program's counters in one map: PIDs are unique across a multiplex.
	///
	/// Every importer reads the same PAT, and programs sharing a PMT PID read the same PMT
	/// sections, so a dropped section counts once however many read it. Every importer also
	/// grades every packet of the multiplex, so each TR 101 290 error counts once too. A
	/// retired program's rows stay until a program carrying the PID again replaces them.
	pub fn stats(&self) -> stats::Snapshot {
		let mut stats = stats::Snapshot {
			streams: self.retired.streams.clone(),
			..Default::default()
		};
		let mut crc_errors = self.retired.crc_errors.clone();
		let mut errors = self.retired.errors.clone();
		for program in &self.programs {
			stats.streams.extend(program.import.stats().streams);
			for (&pid, &count) in program.import.crc_errors() {
				let merged = crc_errors.entry(pid).or_default();
				*merged = (*merged).max(count);
			}
			errors.max(program.import.errors());
		}
		// The importers only see the PATs after the one that started them.
		*crc_errors.entry(Pid::PAT).or_default() += self.scan.crc_error;
		stats.crc_error = crc_errors.values().sum();
		errors.report(&mut stats);
		stats
	}

	/// Finish each program's tracks, then its catalog while it still lists them.
	///
	/// Returns each program's [`Finish`](moq_net::broadcast::Finish), which keeps it servable
	/// until its media is stale; dropping one closes that program. A program retired earlier
	/// has none.
	/// Fails when the input ended before its PAT listed a program or any program declared a track.
	pub fn finish(&mut self) -> anyhow::Result<Vec<moq_net::broadcast::Finish>> {
		anyhow::ensure!(self.listed, "input ended before its PAT listed a program");
		let mut announced = self.announced;
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

/// Reads every whole PAT out of the multiplex, ahead of the importers it starts.
#[derive(Default)]
struct PatScan {
	framer: Framer,
	pat: PatReader,
	/// PAT sections dropped for a bad CRC while no importer read them.
	crc_error: u64,
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;
	use crate::catalog::hang::Container;
	use crate::container::Consumer;
	use crate::container::ts::fixture::Mux;
	use crate::container::ts::import::test::{
		corrupt, fifty_programs, mp2_pes, pat_section, psi_packets, two_programs, two_section_pat,
	};

	const MP2: u8 = 0x03;

	/// The epoch each program publishes under, announced yet or not.
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

	/// One step of a two-program multiplex: a PAT listing `listed`, both programs' PMTs, and an
	/// MP2 PES on each program's audio whether or not the PAT lists it.
	struct Steps {
		cc: [u8; 2],
		pts: u64,
	}

	impl Steps {
		fn step(&mut self, programs: &mut Programs, listed: &[(u16, u16)]) {
			let mut mux = Mux::default();
			mux.pat(listed)
				.pmt(0x100, 1, 0x61, &[(MP2, 0x61)], 0)
				.pmt(0x200, 2, 0x71, &[(MP2, 0x71)], 0);
			for (cc, pid) in self.cc.iter_mut().zip([0x61, 0x71]) {
				mux.out.extend(mp2_pes(pid, *cc, self.pts, [pid as u8; 2]));
				*cc = (*cc + 1) & 0x0f;
			}
			// Past both 72 ms frames.
			self.pts += 18_000;
			programs.decode(&mux.bytes()).unwrap();
		}
	}

	/// A program a later PAT adds publishes, one it drops retires alone, and one it restores
	/// publishes a new instance, while the program no PAT touched runs throughout.
	#[test]
	fn programs_follow_the_pat() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
		let mut steps = Steps {
			cc: [0; 2],
			pts: 90_000,
		};

		steps.step(&mut programs, &[(1, 0x100)]);
		let one = epochs(&programs);
		assert_eq!(one.len(), 1);

		steps.step(&mut programs, &[(1, 0x100), (2, 0x200)]);
		let both = epochs(&programs);
		assert_eq!(both[0], one[0], "program 1 carries on");
		assert_eq!(both[1].0, 2, "the added program publishes");
		let two = programs.programs[1].broadcast.consume();

		steps.step(&mut programs, &[(1, 0x100)]);
		assert_eq!(epochs(&programs), one, "program 1 carries on");
		assert!(two.is_closed(), "the dropped program's broadcast is withdrawn");

		steps.step(&mut programs, &[(1, 0x100), (2, 0x200)]);
		let back = epochs(&programs);
		assert_eq!(back[0], one[0], "program 1 carries on");
		assert_eq!(back[1].0, 2);
		assert_ne!(back[1].1, both[1].1, "the returning program is a new instance");

		let _finished = programs.finish().unwrap();
		let stats = programs.stats();
		assert_eq!(stats.streams[&0x61].units, 8, "program 1 delivered every frame");
		assert_eq!(stats.streams[&0x71].units, 2, "the new instance counts its own frames");
	}

	/// A PAT that drops a program reaches no importer of it, so the multiplex carries on
	/// rather than ending on that program's refusal.
	#[test]
	fn a_dropped_program_does_not_end_the_multiplex() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
		let mut steps = Steps {
			cc: [0; 2],
			pts: 90_000,
		};
		steps.step(&mut programs, &[(1, 0x100), (2, 0x200)]);
		// Fed a packet at a time, so the PAT that drops program 2 completes in a call of its own.
		let mut mux = Mux::default();
		mux.pat(&[(1, 0x100)]);
		mux.out.extend(mp2_pes(0x61, 1, 108_000, [0x61; 2]));
		for packet in mux.bytes().chunks(TsPacket::SIZE) {
			programs.decode(packet).unwrap();
		}
		assert_eq!(epochs(&programs).len(), 1);
		let _finished = programs.finish().unwrap();
	}

	/// A program whose PMT never arrives waits, unannounced, and holds up neither the others
	/// nor the end of the input.
	#[test]
	fn a_program_without_a_pmt_waits_unannounced() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", catalog::Config::default());
		let mut steps = Steps {
			cc: [0; 2],
			pts: 90_000,
		};
		steps.step(&mut programs, &[(1, 0x100), (3, 0x300)]);
		steps.step(&mut programs, &[(1, 0x100), (3, 0x300)]);
		let _finished = programs.finish().unwrap();
		let routes: Vec<_> = programs
			.programs
			.iter()
			.map(|program| (program.number, program.broadcast.route().is_some()))
			.collect();
		assert_eq!(routes, vec![(1, true), (3, false)]);
	}

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

	/// The programs the first whole PAT in a buffer starts.
	fn pat_programs(data: &[u8]) -> Option<Vec<u16>> {
		let mut programs = Programs::new(
			crate::source::produce_origin(),
			"event.hang",
			catalog::Config::default(),
		);
		programs.decode(data).unwrap();
		let numbers: Vec<u16> = programs.programs.iter().map(|program| program.number).collect();
		(!numbers.is_empty()).then_some(numbers)
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

	/// A PAT too big for one packet is read whole, and each importer is handed all of it, so
	/// the program listed in its second packet publishes.
	#[tokio::test]
	async fn a_pat_spanning_two_packets_starts_every_program() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin.clone(), "event.hang", catalog::Config::default());
		let input = fifty_programs();
		for chunk in input.chunks(100) {
			programs.decode(chunk).unwrap();
		}
		let _finished = programs.finish().unwrap();
		assert_eq!(programs.programs.len(), 50);

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

	/// A corrupt PAT starts nothing and is counted; the good repetition after it starts the
	/// program without counting again.
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
		assert!(programs.programs.is_empty());
		assert_eq!(programs.stats().crc_error, 1);

		let mut data = psi_packets(0, &mut cc, &[], &pat_section(&[(1, 0x0100)]));
		data.extend_from_slice(&null);
		programs.decode(&data).unwrap();
		assert_eq!(programs.programs.len(), 1);
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
		assert_eq!(programs.programs.len(), 1);
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
