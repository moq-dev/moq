//! Test support, enabled by the `fuzz` feature: the `qmux` fuzz target body and its seeds.
//!
//! The body lives here rather than in `fuzz/fuzz_targets/` because it drives the crate's
//! private transports and session constructor, and so the tests below replay it on stable.
//! It tracks the crate's internals, so it can change in any release. See `fuzz/README.md` for
//! the workflow.

mod alloc;
mod session;

pub use alloc::Counting;
pub use session::qmux;

use session::{Action, Mode};

use crate::Version;

/// One fuzz target body: it returns whether the input got past the handshake.
pub type Target = fn(&[u8]) -> bool;

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[("qmux", qmux)];

/// One input to start a target from.
pub struct Seed {
	/// The target it feeds, a key of [`TARGETS`].
	pub target: &'static str,
	/// The input, selector byte included.
	pub data: Vec<u8>,
}

/// The starting corpus: well-formed traffic at every version over both framings, the same
/// with the server not reading, and the frames and framing a fuzzer would take longest to find.
pub fn seeds() -> Vec<Seed> {
	let traffic = vec![
		Action::OpenUni,
		Action::Write { stream: 0, len: 3000 },
		Action::Finish { stream: 0 },
		Action::OpenBi,
		Action::Write { stream: 1, len: 20000 },
		Action::Read { stream: 1 },
		Action::Datagram { len: 200 },
		Action::Write { stream: 1, len: 100 },
		Action::Finish { stream: 1 },
		Action::Advance { ticks: 20 },
		Action::Read { stream: 1 },
	];
	let stalled = vec![
		Action::Stall(true),
		Action::OpenBi,
		Action::Write { stream: 0, len: 16384 },
		Action::Write { stream: 0, len: 16384 },
		Action::Datagram { len: 1000 },
		Action::Advance { ticks: 50 },
		Action::Stall(false),
		Action::Read { stream: 0 },
	];
	let closed = vec![
		Action::OpenUni,
		Action::Write { stream: 0, len: 500 },
		Action::Reset { stream: 0, code: 7 },
		Action::OpenBi,
		Action::Stop { stream: 1, code: 3 },
		Action::Close { code: 1 },
	];
	let aborted = vec![Action::OpenBi, Action::Write { stream: 0, len: 10 }, Action::Abort];

	let mut seeds = Vec::new();
	let mut push = |mode: Mode, actions: &[Action]| {
		let mut data = vec![mode.selector()];
		for action in actions {
			action.encode(&mut data);
		}
		seeds.push(Seed { target: "qmux", data });
	};
	for &version in Version::ALL {
		for websocket in [false, true] {
			for reads in [true, false] {
				let mode = Mode {
					version,
					websocket,
					reads,
				};
				push(mode, &traffic);
				if reads {
					push(mode, &stalled);
					push(mode, &closed);
					push(mode, &aborted);
					push(mode, &framing(version));
				}
			}
		}
	}
	seeds
}

/// Frames the peer's own session never sends, and record sizes at and past the limit.
fn framing(version: Version) -> Vec<Action> {
	// STREAM with offset, length, and FIN on the peer's first unidirectional stream (ID 2),
	// then one past the stream limit (ID 2 + 4 * MAX_STREAMS), then MAX_DATA and QX_PING.
	let stream = |id: u8| vec![0x0f, id, 0x00, 0x03, b'a', b'b', b'c'];
	let mut actions = vec![
		Action::Record(stream(2)),
		Action::Record(stream(2 + 4 * 4)),
		Action::Record(vec![0x10, 0x44, 0x00]),
	];
	if version.uses_records() {
		// QX_PING request (0x348c67529ef8c7bd) with sequence 1.
		actions.push(Action::Record(vec![
			0xf4, 0x8c, 0x67, 0x52, 0x9e, 0xf8, 0xc7, 0xbd, 0x01,
		]));
		// A Size field of 16383, one past the largest record, and no record behind it.
		actions.push(Action::Raw(vec![0x7f, 0xff]));
	} else {
		// A STREAM frame without a length, which a byte stream cannot delimit.
		actions.push(Action::Raw(vec![0x08, 0x02, b'x']));
	}
	actions
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every seed gets past the handshake and passes every check, so the fuzzer starts from
	/// sessions that work.
	#[test]
	fn seeds_pass() {
		for (index, seed) in seeds().iter().enumerate() {
			let (_, target) = TARGETS.iter().find(|(name, _)| *name == seed.target).unwrap();
			assert!(target(&seed.data), "seed {index} did not establish");
		}
	}

	/// Replay the inputs committed under `fuzz/regressions/`, so a bug the fuzzer found stays
	/// fixed on the stable toolchain, in the suite CI already runs.
	#[test]
	fn regressions() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regressions");

		for (name, target) in TARGETS {
			// Nothing has failed yet for this target; `fuzz/README.md` says what to drop in here
			// when something does.
			let Ok(entries) = std::fs::read_dir(root.join(name)) else {
				continue;
			};
			for entry in entries {
				let path = entry.expect("could not read the regression directory").path();
				let data = std::fs::read(&path).expect("could not read a regression input");
				target(&data);
			}
		}
	}
}
