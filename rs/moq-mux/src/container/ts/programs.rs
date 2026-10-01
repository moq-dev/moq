//! Every program of a multiplex, each imported as its own broadcast.

use anyhow::Context;
use mpeg2ts::ts::{ReadTsPacket, TsPacket, TsPacketReader, TsPayload};

use super::{Ext, Import, Stats};
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
	live: bool,
	/// Input held until it contains a whole PAT.
	pending: Vec<u8>,
	/// Empty until the PAT arrives.
	programs: Vec<Program>,
}

/// One program's broadcast.
struct Program {
	/// Keeps the origin-created broadcast alive; the importer's clone doesn't.
	_broadcast: moq_net::broadcast::Producer,
	import: Import<Ext>,
	catalog: catalog::Producer<Ext>,
}

impl Programs {
	/// Split the multiplex into broadcasts named after `name`, each catalog built from `config`.
	pub fn new(origin: moq_net::origin::Producer, name: impl moq_net::AsPath, config: catalog::Config) -> Self {
		Self {
			origin,
			name: name.as_path().to_owned(),
			config,
			live: false,
			pending: Vec::new(),
			programs: Vec::new(),
		}
	}

	/// Publish each program on its broadcast's clock, as [`Import::live`] does.
	pub fn live(mut self) -> Self {
		self.live = true;
		self
	}

	/// Demux a chunk of the multiplex. A trailing partial packet is retained for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		if !self.programs.is_empty() {
			return self.feed(data);
		}
		self.pending.extend_from_slice(data);
		let Some(programs) = pat_programs(&self.pending).filter(|programs| !programs.is_empty()) else {
			// Only a packet's worth of tail can still hold the start of the PAT.
			let keep = self.pending.len().saturating_sub(TsPacket::SIZE - 1);
			self.pending.drain(..keep);
			return Ok(());
		};
		for program in programs {
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
			if self.live {
				import = import.live();
			}
			broadcast
				.announce(Default::default())
				.with_context(|| format!("failed to announce broadcast {name}"))?;
			self.programs.push(Program {
				_broadcast: broadcast,
				import,
				catalog,
			});
		}
		let pending = std::mem::take(&mut self.pending);
		self.feed(&pending)
	}

	fn feed(&mut self, data: &[u8]) -> anyhow::Result<()> {
		for program in &mut self.programs {
			program.import.decode(data)?;
		}
		Ok(())
	}

	/// Every program's counters in one map: PIDs are unique across a multiplex.
	pub fn stats(&self) -> Stats {
		let mut stats = Stats::default();
		for program in &self.programs {
			stats.streams.extend(program.import.stats().streams);
		}
		stats
	}

	/// Finish each program's tracks, then its catalog while it still lists them.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		for program in &mut self.programs {
			program.import.finish()?;
			program.catalog.finish()?;
		}
		Ok(())
	}

	/// Abort every program's tracks with `err`, as [`Import::abort`] does. Consumes the importer.
	pub fn abort(self, err: moq_net::Error) {
		for program in self.programs {
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

/// The program numbers listed by the first whole PAT in `data`, in PAT order, or `None` if
/// there is none yet.
fn pat_programs(data: &[u8]) -> Option<Vec<u16>> {
	let mut off = 0;
	while let Some(rel) = memchr::memchr(0x47, &data[off..]) {
		off += rel;
		let packet = data.get(off..off + TsPacket::SIZE)?;
		// PID 0 opening a section. The section CRC rejects a sync byte found in payload.
		if packet[1] & 0x5f == 0x40
			&& packet[2] == 0
			&& let Ok(Some(TsPacket {
				payload: Some(TsPayload::Pat(pat)),
				..
			})) = TsPacketReader::new(packet).read_ts_packet()
		{
			return Some(
				pat.table
					.iter()
					.map(|entry| entry.program_num)
					.filter(|&program| program != 0)
					.collect(),
			);
		}
		off += 1;
	}
	None
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;
	use crate::catalog::hang::Container;
	use crate::container::Consumer;
	use crate::container::ts::import::test::two_programs;

	#[test]
	fn program_broadcasts_keep_the_catalog_suffix_last() {
		assert_eq!(program_broadcast("event.hang", 2), "event/2.hang");
		assert_eq!(program_broadcast("demo/event.msf", 7), "demo/event/7.msf");
		assert_eq!(program_broadcast("event", 1), "event/1");
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

	/// The input is held until the PAT is whole, then each program publishes as its own
	/// broadcast carrying only that program, live on its own first frame rather than an hour
	/// apart on one shared clock.
	#[tokio::test]
	async fn every_program_publishes_its_own_broadcast() {
		let origin = crate::source::produce_origin();
		let ago = Duration::from_secs(60);
		let clock = crate::Clock::at(std::time::Instant::now() - ago, std::time::SystemTime::now() - ago).unwrap();
		let config = catalog::Config::default().with_clock(clock);
		let mut programs = Programs::new(origin.clone(), "event.hang", config).live();

		let input = two_programs();
		let before = clock.now();
		programs.decode(&input[..100]).unwrap();
		assert!(
			programs.programs.is_empty(),
			"no program is known before the PAT is whole"
		);
		programs.decode(&input[100..]).unwrap();
		programs.finish().unwrap();
		let after = clock.now();

		for (path, fills) in [("event/1.hang", [0xAA, 0xBB]), ("event/2.hang", [0xCC, 0xDD])] {
			let consumer = crate::Source::new(origin.consume(), path).broadcast().await.unwrap();
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
			let frame = Consumer::new(track, container).read().await.unwrap().expect("a frame");
			assert!(fills.contains(&frame.payload[4]), "{path} carries only its own program");
			let skew = Duration::from_secs(2).as_micros();
			assert!(
				before.as_micros() - skew <= frame.timestamp.as_micros()
					&& frame.timestamp.as_micros() <= after.as_micros() + skew,
				"{path} is live on arrival: {:?} not in {before:?}..={after:?}",
				frame.timestamp
			);
		}
	}
}
