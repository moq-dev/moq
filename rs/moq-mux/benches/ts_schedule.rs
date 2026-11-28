//! TS admission and layout across PID count and access units per PID, and padded layout
//! across queued tracks and access-unit size.
//!
//! Run with `cargo bench -p moq-mux --bench ts_schedule`.

use std::fmt;
use std::hint::black_box;
use std::time::Duration;

use swole::bench;

mod export {
	pub const PCR_INTERVAL: Duration = Duration::from_millis(25);
	use std::time::Duration;
}
#[path = "../src/container/ts/schedule.rs"]
#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
mod schedule;

fn packet(pid: u16) -> [u8; 188] {
	let mut packet = [0xff; 188];
	packet[..4].copy_from_slice(&[0x47, (pid >> 8) as u8, pid as u8, 0x10]);
	packet
}

/// Pin glibc's heap so every slot reuses it. A padded slot at 100 Mb/s lays out about 300 KB,
/// right at the size where glibc adapts between `mmap` and the heap and trims the heap's
/// top, so which one a run settles on, and with it fresh page faults on every slot, flips
/// with the process's heap history alone: a 5x swing on the ARM runners with no change to the
/// schedule.
fn hold_heap() {
	#[cfg(all(target_os = "linux", target_env = "gnu"))]
	// SAFETY: mallopt only tunes the allocator, called before anything is measured.
	unsafe {
		assert_eq!(libc::mallopt(libc::M_MMAP_THRESHOLD, 32 << 20), 1);
		assert_eq!(libc::mallopt(libc::M_TRIM_THRESHOLD, libc::c_int::MAX), 1);
	}
}

/// Two dimensions, named `<a>/<b>`.
#[derive(Clone, Copy)]
struct Case<A, B> {
	a: A,
	b: B,
}

impl<A: fmt::Display, B: fmt::Display> fmt::Display for Case<A, B> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.a, self.b)
	}
}

/// PIDs and access units per PID.
type Admission = Case<u16, u128>;

fn admissions() -> Vec<Admission> {
	[1u16, 4, 16, 64]
		.into_iter()
		.flat_map(|a| [1u128, 16, 64].map(|b| Case { a, b }))
		.collect()
}

/// The case, once the heap is held for it.
fn held<T>(case: T) -> T {
	hold_heap();
	case
}

fn admission(&Case { a: pids, b: frames }: &Admission, skipped: bool) {
	let mut schedule = schedule::Schedule::new(Duration::from_millis(200));
	schedule.set_rate(Some(100_000_000));
	schedule.set_clock(1);
	for pid in 1..=pids {
		schedule.set_buffer(
			pid,
			schedule::Buffer {
				rate: 2_000_000,
				leak: None,
				size: Some(1_000_000),
				transport: 512,
				reach: Duration::from_secs(1),
			},
		);
	}
	for frame in 0..frames {
		for pid in 1..=pids {
			let gap = if skipped && frame >= frames / 2 { 4_000 } else { 0 };
			let at = (999 + gap + frame * 40) * 1_000_000;
			schedule
				.push_frames(
					pid,
					schedule::Timing {
						by: at,
						decode: at + 1_000_000,
					},
					packet(pid).repeat(10),
					false,
					Vec::new(),
				)
				.unwrap();
		}
	}
	while let Some(slot) = schedule.next(None).unwrap() {
		black_box(slot.layout(&packet(1), 100, &packet(0x1fff)));
	}
}

#[bench(args = admissions(), setup = held)]
fn ts_schedule(case: &Admission) {
	admission(case, false);
}

/// The second half of each PID's units arrive after a gap.
#[bench(args = admissions(), setup = held)]
fn ts_schedule_skip(case: &Admission) {
	admission(case, true);
}

/// Padded layout across queued tracks and access-unit size, each track's first unit with the
/// tables ahead of it, the larger units spanning several slots.
/// Queued tracks and access-unit size in packets.
type Tables = Case<u16, usize>;

fn tables() -> Vec<Tables> {
	[1u16, 4, 16]
		.into_iter()
		.flat_map(|a| [100usize, 1_000, 4_000].map(|b| Case { a, b }))
		.collect()
}

#[bench(args = tables(), setup = held)]
fn ts_schedule_tables(&Case { a: tracks, b: size }: &Tables) {
	let rate = 100_000_000u64;
	let mut schedule = schedule::Schedule::new(Duration::from_secs(2));
	schedule.set_rate(Some(rate));
	schedule.set_clock(1);
	schedule.set_tables(0x1000);
	for pid in 1..=tracks {
		schedule.set_buffer(
			pid,
			schedule::Buffer {
				rate,
				leak: None,
				size: Some(1_000_000),
				transport: 512,
				reach: Duration::from_secs(2),
			},
		);
	}
	// Due one after another, spaced for the multiplex to carry them at about 70 %.
	let spacing = (size as u128 + 2) * 188 * 8 * 1_000_000_000 / u128::from(rate) * 10 / 7;
	for frame in 0..4u128 {
		for pid in 1..=tracks {
			let at = 1_000_000_000 + (frame * u128::from(tracks) + u128::from(pid)) * spacing;
			let mut packets = match frame {
				0 => [packet(0), packet(0x1000)].concat(),
				_ => Vec::new(),
			};
			packets.extend(packet(pid).repeat(size));
			schedule
				.push_frames(
					pid,
					schedule::Timing {
						by: at,
						decode: at + 1_000_000,
					},
					packets,
					frame == 0,
					Vec::new(),
				)
				.unwrap();
		}
	}
	while let Some(slot) = schedule.next(None).unwrap() {
		black_box(slot.layout(&packet(1), 100, &packet(0x1fff)));
	}
}

fn main() {
	swole::main();
}
