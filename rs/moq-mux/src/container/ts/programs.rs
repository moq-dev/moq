//! Every program of a multiplex, each imported as its own broadcast.

use std::collections::BTreeMap;

use anyhow::Context;
use mpeg2ts::ts::{Pid, TsPacket};

use super::import::{Framer, PatReader};
use super::{Ext, Import, psi, stats};
use crate::catalog;

/// Imports every program the first PAT lists, each as its own broadcast on `origin`, with its
/// own clock and catalog.
///
/// Program `n` of `event.hang` publishes on `event/n.hang`, keeping the catalog suffix last so
/// format detection still sees it; a name without one gains a plain `/n`. Input ahead of that
/// PAT is dropped, as a single [`Import`] drops media ahead of its PSI. A program a later PAT
/// adds is not published; one it removes ends the import.
pub struct Programs {
	origin: moq_net::origin::Producer,
	name: moq_net::PathOwned,
	config: catalog::Config,
	/// Input not yet read for the PAT: a trailing partial packet, or what follows the PAT.
	pending: Vec<u8>,
	/// Reads the PAT out of the input.
	scan: PatScan,
	/// Empty until the PAT arrives.
	programs: Vec<Program>,
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
		let Some((route, listed)) = &self.pending else {
			return Ok(true);
		};
		if listed.latest().is_none() {
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
		}
	}

	/// Demux a chunk of the multiplex. A trailing partial packet is retained for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		if !self.programs.is_empty() {
			return self.feed(data);
		}
		self.pending.extend_from_slice(data);
		let Some(pat) = self.scan.scan(&mut self.pending) else {
			return Ok(());
		};
		for program in pat.program_numbers() {
			let name = program_broadcast(self.name.as_str(), program);
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
			let mut import = Import::new(broadcast.clone(), catalog.reserve()).with_program(program);
			// Seeded with the scanner's PAT rather than replaying its packets, so every section
			// reaches the importer and nothing the scanner counted is counted again.
			import.handle_pat(&pat)?;
			// Each program is its own publisher instance for this run, so a restart
			// replaces it rather than resuming into it.
			let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
			let listed = broadcast.consume().track(hang::Catalog::DEFAULT_NAME)?;
			self.programs.push(Program {
				number: program,
				broadcast,
				import,
				catalog,
				pending: Some((route, listed)),
			});
		}
		let pending = std::mem::take(&mut self.pending);
		self.feed(&pending)
	}

	fn feed(&mut self, data: &[u8]) -> anyhow::Result<()> {
		for program in &mut self.programs {
			program.import.decode(data)?;
			program.announce_if_listed()?;
		}
		Ok(())
	}

	/// Every program's counters in one map: PIDs are unique across a multiplex.
	///
	/// Every importer reads the same PAT, and programs sharing a PMT PID read the same PMT
	/// sections, so a dropped section counts once however many read it. Every importer also
	/// grades every packet of the multiplex, so each TR 101 290 error counts once too.
	pub fn stats(&self) -> stats::Snapshot {
		let mut stats = stats::Snapshot::default();
		let mut crc_errors = BTreeMap::<u16, u64>::new();
		let mut errors = super::health::Errors::default();
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
	/// Fails when the input ended before its PAT listed a program or any program declared a track.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		anyhow::ensure!(!self.programs.is_empty(), "input ended before its PAT listed a program");
		let mut announced = false;
		for program in &mut self.programs {
			program.import.finish()?;
			announced |= program.announce_if_listed()?;
			program.catalog.finish()?;
		}
		anyhow::ensure!(announced, "input ended before any program declared a track");
		Ok(())
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

/// Finds the first whole PAT listing a program, before any importer exists to read one.
#[derive(Default)]
struct PatScan {
	framer: Framer,
	pat: PatReader,
	/// PAT sections dropped for a bad CRC before the first whole PAT.
	crc_error: u64,
}

impl PatScan {
	/// Read `pending` up to the first whole PAT listing a program, returning it with `pending`
	/// trimmed to start just after the packet that completed it. Otherwise only a trailing
	/// partial packet is kept: the reader holds a partial PAT itself.
	fn scan(&mut self, pending: &mut Vec<u8>) -> Option<psi::Pat> {
		let mut off = 0;
		let mut found = None;
		while let Some(at) = self.framer.next(pending, &mut off) {
			let pkt: &[u8; TsPacket::SIZE] = pending[at..at + TsPacket::SIZE].try_into().unwrap();
			if (u16::from(pkt[1] & 0x1f) << 8 | u16::from(pkt[2])) != Pid::PAT {
				continue;
			}
			if let Some(pat) = self.pat.push(pkt, &mut self.crc_error)
				&& !pat.program_numbers().is_empty()
			{
				found = Some(pat);
				break;
			}
		}
		pending.drain(..off);
		found
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;
	use crate::catalog::hang::Container;
	use crate::container::Consumer;
	use crate::container::ts::import::test::{
		corrupt, fifty_programs, pat_section, psi_packets, two_programs, two_section_pat,
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
				matches!(err, crate::Error::Json(moq_json::Error::Net(moq_net::Error::Transport(ref reason))) if reason == "decode failed")
			);
		}
	}

	#[test]
	fn program_broadcasts_keep_the_catalog_suffix_last() {
		assert_eq!(program_broadcast("event.hang", 2), "event/2.hang");
		assert_eq!(program_broadcast("demo/event.msf", 7), "demo/event/7.msf");
		assert_eq!(program_broadcast("event", 1), "event/1");
	}

	/// The PAT in a whole buffer, as one [`PatScan`] reads it.
	fn pat_programs(data: &[u8]) -> Option<Vec<u16>> {
		Some(PatScan::default().scan(&mut data.to_vec())?.program_numbers())
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
		programs.finish().unwrap();
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
		programs.finish().unwrap();
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
		let err = programs.finish().expect_err("no PAT");
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

		let err = programs.finish().expect_err("the PMTs have no configured media");
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
		programs.finish().unwrap();
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
