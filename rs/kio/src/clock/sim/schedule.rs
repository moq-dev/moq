//! Which ready task the simulator runs next: the interleavings a run explores.
//!
//! Experimental: chosen per run from `KIO_SIM_SCHEDULE`.
//!
//! - `fifo` (the default): wake order.
//! - `random`: a uniform pick among the ready tasks.
//! - `pct[:<depth>[:<steps>]]`: probabilistic concurrency testing (Burckhardt et al.,
//!   ASPLOS 2010). Every task draws a random priority and the highest ready one runs; at
//!   `depth - 1` steps drawn from the first `steps`, the one running drops below every
//!   initial priority. Defaults to depth 3 over 1000 steps.
//!
//! A task that wakes itself while it runs yields: under `pct` it drops below everything,
//! so a spin loop cannot starve the rest the way a fixed priority would let it. That
//! demotion voids the paper's bound (a bug needing `depth` ordering constraints among `n`
//! tasks and `steps` steps found with probability at least `1 / (n * steps^(depth - 1))`):
//! an order that needs a self-waking task to run twice in a row is never drawn.
//!
//! A [`Tasks`](crate::Tasks) set's children keep the set's own rotating slot order under
//! every policy.
//!
//! Draws come from their own generator, seeded from the run's seed, so a schedule
//! replays bit-exact from its seed and leaves [`rng`](crate::clock::rng) draws unchanged.

use rand::{RngExt, SeedableRng, rngs::StdRng};

/// The order a run explores.
#[derive(Clone, Debug)]
pub(crate) enum Policy {
	Fifo,
	Random,
	Pct { depth: u32, steps: u64 },
}

/// How a run schedules, and whether it hunts lost wakeups.
#[derive(Clone, Debug)]
pub(crate) struct Config {
	pub policy: Policy,
	/// At every stall, re-poll each parked task and child before time moves, and panic
	/// if one makes progress, since nothing woke it.
	///
	/// Every wake must then come from the run's thread.
	pub probe: bool,
}

/// Wake order, without probing.
impl Default for Config {
	fn default() -> Self {
		Self {
			policy: Policy::Fifo,
			probe: false,
		}
	}
}

impl Config {
	/// The config `KIO_SIM_SCHEDULE` and `KIO_SIM_PROBE` name: FIFO without probing when unset.
	pub fn from_env() -> Self {
		let policy = match std::env::var("KIO_SIM_SCHEDULE") {
			Ok(policy) => Policy::parse(&policy).unwrap_or_else(|| {
				panic!("KIO_SIM_SCHEDULE must be fifo, random, or pct[:<depth>[:<steps>]], not {policy:?}")
			}),
			Err(std::env::VarError::NotPresent) => Policy::Fifo,
			Err(err) => panic!("KIO_SIM_SCHEDULE: {err}"),
		};
		let probe = match std::env::var("KIO_SIM_PROBE").as_deref() {
			Ok("1") => true,
			Ok("0") | Err(std::env::VarError::NotPresent) => false,
			Ok(other) => panic!("KIO_SIM_PROBE must be 0 or 1, not {other:?}"),
			Err(err) => panic!("KIO_SIM_PROBE: {err}"),
		};
		Self { policy, probe }
	}
}

/// The environment settings beyond the seed that replay a run, each with a leading space.
/// Both are always named, so a replay ignores whatever the shell already exports.
impl std::fmt::Display for Config {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.policy {
			Policy::Fifo => f.write_str(" KIO_SIM_SCHEDULE=fifo")?,
			Policy::Random => f.write_str(" KIO_SIM_SCHEDULE=random")?,
			Policy::Pct { depth, steps } => write!(f, " KIO_SIM_SCHEDULE=pct:{depth}:{steps}")?,
		}
		write!(f, " KIO_SIM_PROBE={}", u8::from(self.probe))
	}
}

impl Policy {
	fn parse(policy: &str) -> Option<Self> {
		let mut parts = policy.split(':');
		let policy = match parts.next()? {
			"fifo" => Self::Fifo,
			"random" => Self::Random,
			"pct" => {
				let depth = parts.next().map_or(Some(3), |depth| depth.parse().ok())?;
				let steps = parts.next().map_or(Some(1000), |steps| steps.parse().ok())?;
				if depth == 0 || steps == 0 {
					return None;
				}
				Self::Pct { depth, steps }
			}
			_ => return None,
		};
		parts.next().is_none().then_some(policy)
	}
}

/// A task's rank under `pct`; higher runs first.
pub(crate) type Priority = i64;

/// Draws a run's scheduling choices.
pub(crate) struct Scheduler {
	policy: Policy,
	rng: StdRng,
	/// Task polls so far.
	steps: u64,
	/// The `pct` change points still ahead, latest first, with the priority each sets.
	changes: Vec<(u64, Priority)>,
	/// The lowest priority given out, so a yield goes below it.
	lowest: Priority,
}

/// Mixed into the run's seed, so the schedule's draws are not the run's own.
const SALT: u64 = 0x9e37_79b9_7f4a_7c15;

impl Scheduler {
	pub fn new(policy: Policy, seed: u64) -> Self {
		let mut rng = StdRng::seed_from_u64(seed ^ SALT);
		let mut changes = Vec::new();
		if let Policy::Pct { depth, steps } = policy {
			// Change point `i` (from 1) sets priority `depth - i`, below every initial one.
			changes = (1..depth)
				.map(|i| (rng.random_range(0..steps), Priority::from(depth - i)))
				.collect();
			changes.sort_unstable_by(|a, b| b.cmp(a));
		}
		Self {
			policy,
			rng,
			steps: 0,
			changes,
			lowest: 0,
		}
	}

	/// Whether the run keeps wake order.
	pub fn is_fifo(&self) -> bool {
		matches!(self.policy, Policy::Fifo)
	}

	/// A new task's priority, under `pct`, the one policy that ranks tasks.
	pub fn priority(&mut self) -> Option<Priority> {
		match self.policy {
			Policy::Pct { depth, .. } => Some(Priority::from(depth) + Priority::from(self.rng.random::<u32>())),
			_ => None,
		}
	}

	/// Pick one of the first `ready` tasks in wake order, reading a task's priority by its
	/// place only under `pct`, so the other policies pick in constant time.
	pub fn pick(&mut self, ready: usize, priority: impl Fn(usize) -> Priority) -> usize {
		debug_assert!(ready > 0);
		match self.policy {
			Policy::Fifo => 0,
			Policy::Random => self.rng.random_range(0..ready),
			// The first of the highest, so ties go in wake order.
			Policy::Pct { .. } => (0..ready).rev().max_by_key(|&at| priority(at)).expect("a ready task"),
		}
	}

	/// Count a step by the task at `priority`, which woke itself while it ran when `yielded`.
	pub fn ran(&mut self, priority: &mut Priority, yielded: bool) {
		self.steps += 1;
		if !matches!(self.policy, Policy::Pct { .. }) {
			return;
		}
		if yielded {
			self.lowest -= 1;
			*priority = self.lowest;
		}
		while self.changes.last().is_some_and(|&(step, _)| step < self.steps) {
			let (_, changed) = self.changes.pop().expect("a change point");
			*priority = (*priority).min(changed);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn policies_parse_strictly() {
		assert!(matches!(Policy::parse("fifo"), Some(Policy::Fifo)));
		assert!(matches!(Policy::parse("random"), Some(Policy::Random)));
		assert!(matches!(
			Policy::parse("pct"),
			Some(Policy::Pct { depth: 3, steps: 1000 })
		));
		assert!(matches!(
			Policy::parse("pct:2:50"),
			Some(Policy::Pct { depth: 2, steps: 50 })
		));
		for bad in ["", "FIFO", "pct:0", "pct:2:0", "pct:x", "random:1", "pct:1:2:3"] {
			assert!(Policy::parse(bad).is_none(), "{bad:?} parsed");
		}
	}

	/// A replay hint names both settings, defaults included, so the shell's own cannot leak in.
	#[test]
	fn replay_hints_name_every_setting() {
		assert_eq!(Config::default().to_string(), " KIO_SIM_SCHEDULE=fifo KIO_SIM_PROBE=0");
		let config = Config {
			policy: Policy::Pct { depth: 2, steps: 50 },
			probe: true,
		};
		assert_eq!(config.to_string(), " KIO_SIM_SCHEDULE=pct:2:50 KIO_SIM_PROBE=1");
	}

	/// A change point drops the running unit below every initial priority, and a yield
	/// drops it below everything given out so far.
	#[test]
	fn pct_change_points_and_yields_demote() {
		let mut scheduler = Scheduler::new(Policy::Pct { depth: 2, steps: 1 }, 7);
		let mut first = scheduler.priority().unwrap();
		let second = scheduler.priority().unwrap();
		assert!(first >= 2 && second >= 2);
		// The one change point is at step 0, so the first step lands on it.
		scheduler.ran(&mut first, false);
		assert_eq!(first, 1);
		let mut third = scheduler.priority().unwrap();
		scheduler.ran(&mut third, true);
		assert!(third < first, "a yield goes below the change point");
	}
}
