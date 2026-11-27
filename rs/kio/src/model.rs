//! An exhaustive, breadth-first model checker for pure state machines.
//!
//! A [`Machine`] is a value advanced one event at a time by [`Machine::step`], which
//! appends what it asks of its environment to an output list and never blocks, sleeps,
//! or reads a clock. Close it with an abstract environment that lists every event it
//! may offer ([`Explore`]), and [`check`] visits every state reachable from an initial
//! one, checking the machine's [invariant](Machine::invariant), its
//! [step rule](Explore::transition), and its [quiet-state rule](Explore::quiet) on
//! each, and returns the shortest trace to the first rule broken. [`find`] returns the
//! shortest trace to a state a goal holds in instead, a witness that the search reaches
//! the behavior a rule is about.
//!
//! The checker needs only `Clone + Eq + Hash` state, so the machine production runs, with
//! `Vec`, `BTreeMap`, and `u64` fields, checks as it is. The environment bounds the
//! search: offer events that keep the reachable states finite, such as a cap on
//! arrivals, or the search never ends.
//!
//! Like TLC, it keeps a 64-bit fingerprint of every visited state rather than the state
//! itself, so a state costs a fingerprint and the event that reached it once expanded,
//! and it expands states level by level, so the first trace it finds to a broken rule is
//! a shortest one. A trace is replayed from the initial state, as TLC re-explores. As
//! with TLC, "every reachable state" holds up to fingerprint collisions: a state whose
//! fingerprint matches a visited one is skipped, unchecked. For `n` states the chance
//! of any collision is about `n² / 2⁶⁵`, about 2⁻²⁹ at 300 k states. Fingerprints
//! use std's fixed-key hasher, so a run is reproducible on one toolchain. The estimate
//! assumes `Hash` feeds it every field that steps or rules read: states that differ only
//! in a field it skips always share a fingerprint, so all but the first go unchecked.
//!
//! Requires the `model` feature, meant for dev-dependencies as `sim` is.
//!
//! ```
//! use kio::model::{self, Disabled, Explore, Machine};
//!
//! /// A counter that a reset sends back to zero.
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! struct Counter {
//!     n: u8,
//!     cap: u8,
//! }
//!
//! #[derive(Clone, Debug)]
//! enum Event {
//!     Inc,
//!     Reset,
//! }
//!
//! impl Machine for Counter {
//!     type Event = Event;
//!     type Output = ();
//!
//!     fn step(&mut self, event: Event, _: &mut Vec<()>) -> Result<(), Disabled> {
//!         match event {
//!             Event::Inc if self.n == self.cap => return Err(Disabled),
//!             Event::Inc => self.n += 1,
//!             Event::Reset => self.n = 0,
//!         }
//!         Ok(())
//!     }
//!
//!     fn invariant(&self) -> Result<(), &'static str> {
//!         if self.n > 3 { Err("AtMostThree") } else { Ok(()) }
//!     }
//! }
//!
//! impl Explore for Counter {
//!     fn events(&self, out: &mut Vec<Event>) {
//!         out.extend([Event::Inc, Event::Reset]);
//!     }
//! }
//!
//! let stats = model::check(Counter { n: 0, cap: 3 }).unwrap();
//! assert_eq!(stats.distinct, 4);
//!
//! let violation = model::check(Counter { n: 0, cap: 5 }).unwrap_err();
//! assert_eq!(violation.rule, "AtMostThree");
//! assert_eq!(violation.trace.steps.len(), 4);
//! ```

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::hash::{BuildHasherDefault, Hash, Hasher};

/// A pure state machine: a value advanced one event at a time.
pub trait Machine: Clone + Eq + Hash {
	/// What the environment tells the machine.
	type Event: Clone + fmt::Debug;
	/// What the machine asks of the environment.
	type Output: fmt::Debug;

	/// Apply `event`, appending the machine's requests to `out`, or refuse it, leaving the
	/// machine and `out` as they were, where it cannot happen.
	fn step(&mut self, event: Self::Event, out: &mut Vec<Self::Output>) -> Result<(), Disabled>;

	/// Check the rules every reachable state keeps, naming the first one broken.
	fn invariant(&self) -> Result<(), &'static str> {
		Ok(())
	}
}

/// An event that cannot happen in the state it was offered to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Disabled;

/// A machine composed with its environment, closed so the checker can enumerate its steps.
pub trait Explore: Machine {
	/// Push every event that may be enabled here; [`Machine::step`] refuses the rest.
	fn events(&self, out: &mut Vec<Self::Event>);

	/// Check a rule about one step from `self` to `next`, as a TLA+ action property does.
	fn transition(&self, next: &Self) -> Result<(), &'static str> {
		let _ = next;
		Ok(())
	}

	/// Check a rule about a state where no event is enabled, such as what it may leave undone.
	fn quiet(&self) -> Result<(), &'static str> {
		Ok(())
	}
}

/// What a complete search covered.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
	/// States visited, each once.
	pub distinct: usize,
	/// Steps taken, counting each step into an already visited state.
	pub generated: usize,
	/// The longest of the shortest paths to a state, in steps.
	pub depth: usize,
	/// The most states waiting to be expanded at once.
	pub queue: usize,
}

/// A path from the initial state, each step with the state it reached.
#[non_exhaustive]
pub struct Trace<M: Machine> {
	/// Where the path starts.
	pub init: M,
	/// Each event and the state after it.
	pub steps: Vec<(M::Event, M)>,
}

impl<M: Machine + fmt::Debug> fmt::Display for Trace<M> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "0: {:?}", self.init)?;
		for (i, (event, state)) in self.steps.iter().enumerate() {
			writeln!(f, "{}: {event:?}\n   {state:?}", i + 1)?;
		}
		Ok(())
	}
}

/// A broken rule, and a shortest path that breaks it.
#[non_exhaustive]
pub struct Violation<M: Machine> {
	/// The rule's name.
	pub rule: &'static str,
	/// The path; for a step rule its last step is the one that breaks it.
	pub trace: Trace<M>,
}

impl<M: Machine + fmt::Debug> fmt::Debug for Violation<M> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"{} broken after {} steps:\n{}",
			self.rule,
			self.trace.steps.len(),
			self.trace
		)
	}
}

/// Visit every state reachable from `init`, checking every rule, and stop at the first
/// broken one.
pub fn check<M: Explore>(init: M) -> Result<Stats, Violation<M>> {
	match search(init, true, |_, _| false) {
		Search::Done(stats) => Ok(stats),
		Search::Broken(violation) => Err(violation),
		Search::Found(_) => unreachable!("nothing to find"),
	}
}

/// A shortest path from `init` to a state `goal` holds in, told whether no event is enabled
/// there; `None` once every reachable state is visited without one. A witness: TLC's
/// expected-to-fail invariant. Rules broken on the way are ignored.
pub fn find<M: Explore>(init: M, goal: impl Fn(&M, bool) -> bool) -> Option<Trace<M>> {
	match search(init, false, goal) {
		Search::Found(trace) => Some(trace),
		Search::Done(_) | Search::Broken(_) => None,
	}
}

enum Search<M: Machine> {
	Done(Stats),
	Broken(Violation<M>),
	Found(Trace<M>),
}

/// Each visited state's way in: its parent's index and the event from there.
struct Node<E> {
	parent: u32,
	event: Option<E>,
}

/// Visit every state reachable from `init` until `goal` holds in one, checking the rules if
/// `rules`.
fn search<M: Explore>(init: M, rules: bool, goal: impl Fn(&M, bool) -> bool) -> Search<M> {
	let mut seen: HashSet<u64, BuildHasherDefault<Fingerprint>> = HashSet::default();
	let mut nodes: Vec<Node<M::Event>> = Vec::new();
	let mut queue: VecDeque<(M, u32)> = VecDeque::new();
	let mut stats = Stats {
		distinct: 1,
		generated: 0,
		depth: 0,
		queue: 1,
	};

	if rules && let Err(rule) = init.invariant() {
		return broken(&init, &[], 0, None, rule);
	}
	seen.insert(fingerprint(&init));
	nodes.push(Node { parent: 0, event: None });
	queue.push_back((init.clone(), 0));

	let mut events = Vec::new();
	let mut out = Vec::new();
	// The index of the first node a level deeper than the one being expanded.
	let mut level_end = 1;
	// A rule broken by a step out of this level, reported once the level's quiet states,
	// whose traces are a step shorter, are checked.
	let mut deeper = None;
	while let Some((state, index)) = queue.pop_front() {
		if index as usize >= level_end {
			if let Some(broken) = deeper {
				return broken;
			}
			stats.depth += 1;
			level_end = nodes.len();
		}
		events.clear();
		state.events(&mut events);
		let mut quiet = true;
		for event in events.drain(..) {
			let mut next = state.clone();
			out.clear();
			if next.step(event.clone(), &mut out).is_err() {
				continue;
			}
			quiet = false;
			stats.generated += 1;
			if deeper.is_some() {
				continue;
			}
			if rules && let Err(rule) = state.transition(&next) {
				deeper = Some(broken(&init, &nodes, index, Some(event), rule));
				continue;
			}
			if !seen.insert(fingerprint(&next)) {
				continue;
			}
			let Ok(child) = u32::try_from(nodes.len()) else {
				panic!("more than 2^32 states");
			};
			nodes.push(Node {
				parent: index,
				event: Some(event),
			});
			stats.distinct += 1;
			if rules && let Err(rule) = next.invariant() {
				deeper = Some(broken(&init, &nodes, child, None, rule));
				continue;
			}
			queue.push_back((next, child));
		}
		stats.queue = stats.queue.max(queue.len());
		if goal(&state, quiet) {
			return Search::Found(trace(&init, &nodes, index, None));
		}
		if rules
			&& quiet
			&& let Err(rule) = state.quiet()
		{
			return broken(&init, &nodes, index, None, rule);
		}
	}
	deeper.unwrap_or(Search::Done(stats))
}

fn broken<M: Explore>(
	init: &M,
	nodes: &[Node<M::Event>],
	index: u32,
	last: Option<M::Event>,
	rule: &'static str,
) -> Search<M> {
	Search::Broken(Violation {
		rule,
		trace: trace(init, nodes, index, last),
	})
}

/// Replay the path to node `index` from `init`, then `last` if any.
fn trace<M: Machine>(init: &M, nodes: &[Node<M::Event>], index: u32, last: Option<M::Event>) -> Trace<M> {
	let mut events: Vec<M::Event> = last.into_iter().collect();
	let mut at = index as usize;
	while let Some(node) = nodes.get(at)
		&& let Some(event) = &node.event
	{
		events.push(event.clone());
		at = node.parent as usize;
	}
	events.reverse();

	let mut state = init.clone();
	let mut out = Vec::new();
	let steps = events
		.into_iter()
		.map(|event| {
			// Each step starts from an empty list, as in the search.
			out.clear();
			state.step(event.clone(), &mut out).expect("a recorded step replays");
			(event, state.clone())
		})
		.collect();
	Trace {
		init: init.clone(),
		steps,
	}
}

fn fingerprint<M: Hash>(state: &M) -> u64 {
	// Fixed keys, unlike `RandomState`, so a run is reproducible.
	let mut hasher = std::hash::DefaultHasher::new();
	state.hash(&mut hasher);
	hasher.finish()
}

/// The visited set's hasher: a fingerprint is already a hash.
#[derive(Default)]
struct Fingerprint(u64);

impl Hasher for Fingerprint {
	fn finish(&self) -> u64 {
		self.0
	}

	fn write(&mut self, _: &[u8]) {
		unreachable!("only fingerprints are hashed");
	}

	fn write_u64(&mut self, fingerprint: u64) {
		self.0 = fingerprint;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A walk along a fixed directed graph, each event the node it moves to, whose rules
	/// break at chosen nodes.
	#[derive(Clone, Debug, PartialEq, Eq, Hash)]
	struct Graph {
		at: u8,
		edges: &'static [(u8, u8)],
		/// Each rule that breaks, and the node it breaks at.
		broken: &'static [(Rule, u8)],
	}

	#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
	enum Rule {
		Invariant,
		/// A step into the node.
		Transition,
		Quiet,
	}

	/// Two paths from 0 to 3, then on to 4.
	const DIAMOND: &[(u8, u8)] = &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 4)];

	impl Graph {
		fn new(edges: &'static [(u8, u8)], broken: &'static [(Rule, u8)]) -> Self {
			Self { at: 0, edges, broken }
		}

		fn breaks(&self, rule: Rule) -> bool {
			self.broken.contains(&(rule, self.at))
		}
	}

	impl Machine for Graph {
		type Event = u8;
		type Output = u8;

		fn step(&mut self, to: u8, out: &mut Vec<u8>) -> Result<(), Disabled> {
			// The checker hands each step an empty list.
			assert!(out.is_empty());
			if !self.edges.contains(&(self.at, to)) {
				return Err(Disabled);
			}
			self.at = to;
			out.push(to);
			Ok(())
		}

		fn invariant(&self) -> Result<(), &'static str> {
			if self.breaks(Rule::Invariant) {
				Err("invariant")
			} else {
				Ok(())
			}
		}
	}

	impl Explore for Graph {
		fn events(&self, out: &mut Vec<u8>) {
			// Every node, so the machine refuses most.
			out.extend(0..8);
		}

		fn transition(&self, next: &Self) -> Result<(), &'static str> {
			if next.breaks(Rule::Transition) {
				Err("transition")
			} else {
				Ok(())
			}
		}

		fn quiet(&self) -> Result<(), &'static str> {
			if self.breaks(Rule::Quiet) { Err("quiet") } else { Ok(()) }
		}
	}

	fn path(trace: &Trace<Graph>) -> Vec<u8> {
		trace.steps.iter().map(|(to, _)| *to).collect()
	}

	#[test]
	fn visits_each_state_once() {
		let stats = check(Graph::new(DIAMOND, &[])).unwrap();
		// Each edge is one step, the second step into 3 included.
		assert_eq!(
			stats,
			Stats {
				distinct: 5,
				generated: 5,
				depth: 3,
				queue: 2,
			}
		);
	}

	#[test]
	fn reports_a_shortest_trace() {
		for broken in [&[(Rule::Invariant, 4)], &[(Rule::Transition, 4)]] {
			let violation = check(Graph::new(DIAMOND, broken)).unwrap_err();
			// Breadth first, so the first of the two shortest paths, in event order.
			assert_eq!(path(&violation.trace), [1, 3, 4], "{violation:?}");
		}
	}

	#[test]
	fn breaks_an_initial_invariant_with_no_steps() {
		let violation = check(Graph::new(DIAMOND, &[(Rule::Invariant, 0)])).unwrap_err();
		assert_eq!(violation.rule, "invariant");
		assert!(violation.trace.steps.is_empty());
	}

	#[test]
	fn checks_quiet_only_where_no_event_is_enabled() {
		check(Graph::new(DIAMOND, &[(Rule::Quiet, 3)])).unwrap();
		let violation = check(Graph::new(DIAMOND, &[(Rule::Quiet, 4)])).unwrap_err();
		assert_eq!(violation.rule, "quiet");
		assert_eq!(path(&violation.trace), [1, 3, 4]);
	}

	#[test]
	fn reports_a_quiet_state_before_a_longer_step_trace() {
		// Node 1, expanded first, steps into 3, breaking a rule a step deeper than quiet 2.
		const FORK: &[(u8, u8)] = &[(0, 1), (0, 2), (1, 3)];
		for broken in [
			&[(Rule::Invariant, 3), (Rule::Quiet, 2)],
			&[(Rule::Transition, 3), (Rule::Quiet, 2)],
		] {
			let violation = check(Graph::new(FORK, broken)).unwrap_err();
			assert_eq!(violation.rule, "quiet", "{violation:?}");
			assert_eq!(path(&violation.trace), [2]);
		}
	}

	#[test]
	fn finds_a_witness_or_none() {
		let end = |g: &Graph, quiet: bool| quiet && g.at == 4;
		assert_eq!(path(&find(Graph::new(DIAMOND, &[]), end).unwrap()), [1, 3, 4]);
		// Rules are ignored on the way.
		assert!(find(Graph::new(DIAMOND, &[(Rule::Invariant, 3)]), end).is_some());
		assert!(find(Graph::new(DIAMOND, &[]), |g, _| g.at == 7).is_none());
	}
}
