//! Bus cost per 10 ms block along both axes: sources and mixes.
//!
//! - `bus_sources`: one mix of every source, swept over the source count and
//!   whether the sources need resampling.
//! - `bus_mixes`: 16 sources, swept over the mix count. `full` routes every
//!   source into every mix; `sparse` routes one source into each, which should
//!   cost little more than the sources alone.
//! - `bus_minus`: a program of every source plus a mix-minus per source, the
//!   per-guest return feed, swept over the source count and how many sources
//!   each minus removes. It should grow with the source count rather than its
//!   square, and with the removed count only once a minus removes most sources.
//!
//! Run with `cargo bench -p moq-audio --bench bus`.

use std::fmt;
use std::hint::black_box;

use moq_audio::bus::{Bus, Config, Driver, Live, Minus, Mix, Strip};
use moq_audio::encode::Input;
use moq_audio::{Format, Layout};

/// The bus's 10 ms block at its default 48 kHz.
const BLOCK: usize = 480;

/// A bus with its sources, one block of PCM for each, and the mixes routed from them.
struct Sources {
	bus: Bus,
	driver: Driver,
	sources: Vec<(Live, Strip)>,
	block: Vec<u8>,
	_mixes: Vec<Mix>,
	_minuses: Vec<Minus>,
}

/// A bus with `count` stereo sources at `rate`, opened by a first advance, and
/// one block of PCM for each.
fn sources(count: usize, rate: u32) -> Sources {
	let (bus, mut driver) = Bus::new(Config::default()).unwrap();
	let frames = BLOCK * rate as usize / 48_000;
	let block: Vec<u8> = std::iter::repeat_n(0.25f32, frames * 2)
		.flat_map(f32::to_le_bytes)
		.collect();
	let sources = (0..count)
		.map(|_| {
			let mut input = Input::new(rate, Layout::Stereo);
			input.format = Format::F32;
			let strip = Strip::default();
			(bus.live(input, strip.clone()).unwrap(), strip)
		})
		.collect();
	// Writes before the first advance are dropped, so open the channels first.
	driver.advance(BLOCK);
	Sources {
		bus,
		driver,
		sources,
		block,
		_mixes: Vec::new(),
		_minuses: Vec::new(),
	}
}

/// A mix of every source.
fn program(sources: &Sources) -> Mix {
	let program = sources.bus.mix();
	for (_, strip) in &sources.sources {
		program.route(strip);
	}
	program
}

fn run(sources: &mut Sources) {
	for (channel, _) in sources.sources.iter_mut() {
		channel.write(black_box(&sources.block)).unwrap();
	}
	sources.driver.advance(black_box(BLOCK));
}

/// A source rate and count.
#[derive(Clone, Copy)]
struct Rated {
	rate: u32,
	count: usize,
}

impl fmt::Display for Rated {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}hz/{}", self.rate, self.count)
	}
}

fn rated() -> Vec<Rated> {
	[48_000u32, 44_100]
		.into_iter()
		.flat_map(|rate| [1usize, 4, 16, 64].map(|count| Rated { rate, count }))
		.collect()
}

fn mixed(case: Rated) -> Sources {
	let mut sources = sources(case.count, case.rate);
	sources._mixes.push(program(&sources));
	sources
}

#[swole::bench(args = rated(), setup = mixed, items = |_: Rated| BLOCK)]
fn bus_sources(sources: &mut Sources) {
	run(sources)
}

/// How each mix routes the sources, and how many mixes.
#[derive(Clone, Copy)]
struct Routed {
	routing: &'static str,
	count: usize,
}

impl fmt::Display for Routed {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.routing, self.count)
	}
}

fn routed() -> Vec<Routed> {
	["full", "sparse"]
		.into_iter()
		.flat_map(|routing| [1usize, 4, 16, 64].map(|count| Routed { routing, count }))
		.collect()
}

fn mixes(case: Routed) -> Sources {
	let mut sources = sources(16, 48_000);
	sources._mixes = (0..case.count)
		.map(|index| {
			let mix = sources.bus.mix();
			match case.routing {
				"full" => sources.sources.iter().for_each(|(_, strip)| mix.route(strip)),
				_ => mix.route(&sources.sources[index % sources.sources.len()].1),
			}
			mix
		})
		.collect();
	sources
}

#[swole::bench(args = routed(), setup = mixes, items = |_: Routed| BLOCK)]
fn bus_mixes(sources: &mut Sources) {
	run(sources)
}

/// How many sources each minus removes, and how many sources.
#[derive(Clone, Copy)]
struct Removed {
	removed: usize,
	count: usize,
}

impl fmt::Display for Removed {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "remove{}/{}", self.removed, self.count)
	}
}

fn removed() -> Vec<Removed> {
	[1usize, 4, 16]
		.into_iter()
		.flat_map(|removed| [16usize, 64].map(|count| Removed { removed, count }))
		.collect()
}

fn minuses(case: Removed) -> Sources {
	let mut sources = sources(case.count, 48_000);
	let program = program(&sources);
	// Each guest's minus removes itself and the next `removed - 1` sources.
	sources._minuses = (0..case.count)
		.map(|guest| program.minus((0..case.removed).map(|offset| &sources.sources[(guest + offset) % case.count].1)))
		.collect();
	sources._mixes.push(program);
	sources
}

#[swole::bench(args = removed(), setup = minuses, items = |_: Removed| BLOCK)]
fn bus_minus(sources: &mut Sources) {
	run(sources)
}

fn main() {
	swole::main();
}
