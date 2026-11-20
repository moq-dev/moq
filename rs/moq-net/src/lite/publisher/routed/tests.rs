//! The lite-07 answer checked over random grant changes, claims, mounts, and edits,
//! interleaved with the order its node keys' output is sent in.
//!
//! Two promises, each judged as a peer sees the stream:
//!
//! - When a prefix's SYNCED goes out, every claim the grant in effect shows under it has had
//!   its Start. The grant in effect is the one whose change the session's key answered last;
//!   a change queued behind it may only take claims away. A claim is owed only when it was
//!   there before the read that tells it started: the edit adding the prefix, or the grant
//!   change in effect. What a fresh answer under that grant tells for the prefix is what the
//!   grant shows.
//! - A claim that leaves while its publisher still announces it ends as lost, never as
//!   withdrawn.
//!
//! `MOQ_ANSWER_SEED` and `MOQ_ANSWER_CASES` pick the cases.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestRunner};

use super::Answer;
use crate::coding::Decode;
use crate::lite::{self, AnnounceBroadcast, Version, subscriber::route_stream::Writer, test_transport::*};
use crate::model::ProduceTest;
use crate::{Hop, Instance, Path, Pattern, Patterns, origin};

/// Where claims are announced: some above what grants and prefixes name, some deeper than a
/// grant's `*` shows, one hidden, and some beneath the mount target `s`.
const PATHS: &[&str] = &["a", "a/x", "a/x/y", "a/.h", "a/m/x", "b", "b/c", "b/c/d", "s", "s/x"];

/// The scopes claims are announced through, which bound what each may serve.
const SCOPES: &[&[&str]] = &[&["**"], &["*/x"]];

/// The grants the peer gives: widen, narrow, move, and revoke between them.
const GRANTS: &[&[&str]] = &[
	&["**"],
	&["a/**"],
	&["b/**"],
	&["a/x/**"],
	&["a/*"],
	&["a/m/**"],
	&["a/**", "b/**"],
	&["a/**", "b/*"],
	&[],
];

/// The prefixes the peer asks for.
const PREFIXES: &[&str] = &["", "a", "a/x", "b", "a/m", "s"];

/// The handle's mount, if any: its point and target.
const MOUNTS: &[Option<(&str, &str)>] = &[None, Some(("a", "s")), Some(("a/m", "s"))];

/// The handle's own scope.
const HANDLES: &[&[&str]] = &[&["**"], &["a/**", "b/**", "s/**"]];

fn path(path: &str) -> Path {
	Path::new(path).unwrap()
}

fn patterns(members: &[&str]) -> Patterns {
	members
		.iter()
		.map(|member| member.parse::<Pattern>().unwrap())
		.collect()
}

fn grant(index: usize) -> crate::auth::Grant {
	crate::auth::Grant {
		publish: Patterns::default(),
		subscribe: patterns(GRANTS[index]),
		expires: None,
	}
}

#[derive(Clone, Debug)]
enum Op {
	/// The grant changes and the session notices.
	Grant(usize),
	/// The grant changes, noticed only by the peer's next edit.
	Quiet(usize),
	Announce {
		path: usize,
		scope: usize,
	},
	Withdraw(usize),
	Reprice {
		path: usize,
		cost: u64,
	},
	/// Removes the prefixes in effect whose bit is set, and adds these.
	Edit {
		remove: u8,
		add: Vec<(usize, bool)>,
	},
	/// Send one thing the session's key queued.
	Main,
	/// Send everything the session's key queued.
	Drain,
	/// Send one thing a mount queued.
	Mount(usize),
}

#[derive(Clone, Debug)]
struct Case {
	mount: usize,
	handle: usize,
	initial: Option<usize>,
	/// Claims announced before anything else, each a path and a scope.
	claims: Vec<(usize, usize)>,
	ops: Vec<Op>,
	/// Whether what is left goes out the mounts' first, rather than the session's key's
	/// first as a session sends it.
	mounts_first: bool,
}

/// A prefix to ask for, the root most often: it holds every region a grant moves between.
fn prefix() -> impl Strategy<Value = usize> {
	prop_oneof![2 => Just(0), 3 => 1..PREFIXES.len()]
}

fn op() -> impl Strategy<Value = Op> {
	prop_oneof![
		6 => (0..GRANTS.len()).prop_map(Op::Grant),
		1 => (0..GRANTS.len()).prop_map(Op::Quiet),
		2 => (0..PATHS.len(), 0..SCOPES.len()).prop_map(|(path, scope)| Op::Announce { path, scope }),
		1 => (0..PATHS.len()).prop_map(Op::Withdraw),
		4 => (0..PATHS.len(), 1..4u64).prop_map(|(path, cost)| Op::Reprice { path, cost }),
		3 => (any::<u8>(), proptest::collection::vec((prefix(), any::<bool>()), 0..3))
			.prop_map(|(remove, add)| Op::Edit { remove, add }),
		5 => Just(Op::Main),
		2 => Just(Op::Drain),
		3 => (0..4usize).prop_map(Op::Mount),
	]
}

fn case() -> impl Strategy<Value = Case> {
	(
		prop_oneof![1 => Just(0), 4 => 1..MOUNTS.len()],
		0..HANDLES.len(),
		proptest::option::weighted(0.8, 0..GRANTS.len()),
		proptest::collection::vec((0..PATHS.len(), 0..SCOPES.len()), 0..6),
		proptest::collection::vec(op(), 1..24),
		any::<bool>(),
	)
		.prop_map(|(mount, handle, initial, claims, ops, mounts_first)| Case {
			mount,
			handle,
			initial,
			claims,
			ops,
			mounts_first,
		})
}

/// A claim as the peer holds it: its path from the session's root, and its instance.
type Held = (Path, Instance);

/// The announce stream's bytes, decoded as the peer reads them.
struct Peer {
	writes: Arc<Mutex<Vec<u8>>>,
	cursor: usize,
	decoder: lite::AnnounceDecoder,
	/// The claims it holds, by Announce ID.
	live: BTreeMap<u64, Held>,
}

impl Peer {
	fn new(log: &Log) -> Self {
		Self {
			writes: log.writes.clone(),
			cursor: 0,
			decoder: lite::AnnounceDecoder::default(),
			live: BTreeMap::new(),
		}
	}

	/// Every message written since the last call.
	fn take(&mut self) -> Vec<AnnounceBroadcast> {
		let buf = self.writes.lock().unwrap()[self.cursor..].to_vec();
		let mut slice = &buf[..];
		let mut msgs = Vec::new();
		while !slice.is_empty() {
			let msg = crate::coding::decode_buf(&mut slice, Version::Lite07, |r, v| AnnounceBroadcast::decode(r, v));
			msgs.push(msg.expect("announce message"));
		}
		self.cursor += buf.len();
		msgs
	}

	fn start(&mut self, suffix: &lite::PathRef, instance: &Instance) -> Held {
		let path = self
			.decoder
			.start(Some(suffix.clone()))
			.unwrap()
			.expect("a path the peer holds");
		let held = (path, instance.clone());
		self.live.insert(self.decoder.last(), held.clone());
		held
	}

	fn end(&mut self, id: u64) -> Held {
		self.decoder.end(id).unwrap();
		self.live.remove(&id).expect("ends a live claim")
	}

	/// Both ends drop every claim within a prefix the edit removed.
	fn drop_within(&mut self, removed: &[Path]) {
		let gone: Vec<u64> = (self.live.iter())
			.filter(|(_, (path, _))| removed.iter().any(|prefix| path.has_prefix(prefix)))
			.map(|(id, _)| *id)
			.collect();
		for id in gone {
			self.end(id);
		}
	}

	fn holds(&self) -> BTreeSet<Held> {
		self.live.values().cloned().collect()
	}
}

/// A writer to a fresh log, and the peer reading it.
fn stream() -> (Writer, Peer) {
	let log = Log::default();
	let peer = Peer::new(&log);
	let writer = Writer::new(
		crate::transport::boxed::SendStream::new(SinkSend::new(log)),
		Version::Lite07,
	);
	(writer, peer)
}

fn flush(writer: &mut Writer) {
	assert!(writer.poll_flush(&kio::Waiter::noop()).is_ready());
}

/// What a fresh answer on `consumer` under `grant` reads and tells for `prefix`, once it settles.
struct View {
	reads: BTreeSet<Path>,
	shown: BTreeSet<Held>,
}

/// What a fresh answer on `consumer` under `grant` reads and tells for `prefix`, once it settles.
fn shown(consumer: &origin::Consumer, grant: Option<usize>, prefix: &Path, hidden: bool) -> View {
	let auth = crate::auth::Link::new(false);
	if let Some(index) = grant {
		auth.handle().authorize(&self::grant(index));
	}
	let mut answer = Answer::alone_on(consumer.clone(), auth);
	let (mut writer, mut peer) = stream();
	answer
		.request(false, Vec::new(), vec![(prefix.clone(), hidden)])
		.unwrap();
	answer.drain(&mut writer).unwrap();
	// What it settles to, judged again from scratch, so its own SYNCED proves nothing.
	answer.resync(&mut writer).unwrap();
	flush(&mut writer);
	for msg in peer.take() {
		match msg {
			AnnounceBroadcast::Start { suffix, instance, .. } => {
				peer.start(&suffix, &instance);
			}
			AnnounceBroadcast::End { id, .. } => {
				peer.end(id);
			}
			_ => {}
		}
	}
	let reads = (answer.prefixes.values().next())
		.map(|prefix| prefix.reads.clone())
		.unwrap_or_default();
	View {
		reads,
		shown: peer.holds(),
	}
}

/// A prefix in effect: its path, its Hidden, and the step that asked for it.
struct Asked {
	path: Path,
	hidden: bool,
	at: usize,
}

/// An edit not answered yet: the Prefix IDs it removes, the prefixes it adds, and its step.
struct Edit {
	remove: Vec<u64>,
	add: Vec<(u64, Path, bool)>,
	at: usize,
}

/// A claim the origin holds: its route, instance, and the step that announced it.
struct Claimed {
	route: crate::model::AnnounceProducer,
	instance: Instance,
	at: usize,
}

struct Sim {
	origin: origin::Producer,
	consumer: origin::Consumer,
	auth: crate::auth::Link,
	answer: Answer,
	writer: Writer,
	peer: Peer,
	/// The step of each op that is no drain, so a claim's age compares with a read's.
	step: usize,
	claims: BTreeMap<usize, Claimed>,
	next_instance: u64,
	/// The grant the answer started from, then each change it queued, with its step.
	grants: Vec<(usize, Option<usize>)>,
	/// The grant last given, queued or not.
	given: Option<usize>,
	prefixes: BTreeMap<u64, Asked>,
	next_prefix: u64,
	edit: Option<Edit>,
	/// The prefixes in effect whose SYNCED went out.
	synced: BTreeSet<u64>,
}

impl Sim {
	fn new(case: &Case) -> Self {
		let origin = Hop::new(1).unwrap().produce();
		let handle = patterns(HANDLES[case.handle]);
		let consumer = match MOUNTS[case.mount] {
			Some((at, target)) => origin.mount(at, target).unwrap().scope("", &handle).unwrap().consume(),
			None => origin.consume().scope("", &handle).unwrap(),
		};
		let auth = crate::auth::Link::new(false);
		if let Some(index) = case.initial {
			auth.handle().authorize(&grant(index));
		}
		let answer = Answer::alone_on(consumer.clone(), auth.clone());
		let (writer, peer) = stream();
		Self {
			origin,
			consumer,
			auth,
			answer,
			writer,
			peer,
			step: 0,
			claims: BTreeMap::new(),
			next_instance: 0,
			grants: vec![(0, case.initial)],
			given: case.initial,
			prefixes: BTreeMap::new(),
			next_prefix: 0,
			edit: None,
			synced: BTreeSet::new(),
		}
	}

	/// How many grant changes the session's key queued and has not answered.
	fn unanswered(&self) -> usize {
		(self.answer.markers.iter())
			.filter(|marker| matches!(marker, super::Marker::Grant { .. }))
			.count()
	}

	/// Run `f`, recording a grant change it queued.
	fn queues<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
		let before = self.unanswered();
		let out = f(self);
		if self.unanswered() > before {
			self.grants.push((self.step, self.given));
		}
		out
	}

	fn apply(&mut self, op: &Op) -> Result<(), TestCaseError> {
		if !matches!(op, Op::Main | Op::Drain | Op::Mount(_)) {
			self.step += 1;
		}
		match op {
			Op::Grant(index) | Op::Quiet(index) => {
				self.given = Some(*index);
				self.auth.handle().authorize(&grant(*index));
				if matches!(op, Op::Grant(_)) {
					self.queues(|sim| sim.answer.poll_permit(&kio::Waiter::noop()))
						.map_err(|err| TestCaseError::fail(format!("poll_permit: {err:?}")))?;
				}
			}
			Op::Announce { path: index, scope } => {
				if self.claims.contains_key(index) {
					return Ok(());
				}
				let Ok(producer) = self.origin.scope("", &patterns(SCOPES[*scope])) else {
					return Ok(());
				};
				self.next_instance += 1;
				let instance: Instance = format!("i{}", self.next_instance).parse().unwrap();
				let info = origin::RouteInfo::new().with_instance(instance.clone(), 0);
				if let Ok(route) = producer.announce(PATHS[*index], info) {
					let at = self.step;
					self.claims.insert(*index, Claimed { route, instance, at });
				}
			}
			Op::Withdraw(index) => {
				self.claims.remove(index);
			}
			Op::Reprice { path: index, cost } => {
				if let Some(held) = self.claims.get(index) {
					let info = origin::RouteInfo::new()
						.with_instance(held.instance.clone(), 0)
						.with_cost(*cost);
					held.route.update(info).unwrap();
				}
			}
			Op::Edit { remove, add } => {
				if self.edit.is_some() {
					return Ok(());
				}
				let remove: Vec<u64> = (self.prefixes.keys().enumerate())
					.filter(|(bit, _)| *bit < 8 && remove & (1 << bit) != 0)
					.map(|(_, id)| *id)
					.collect();
				// Only what a peer may ask for: no prefix twice, or at or below another in the set.
				let mut set: Vec<Path> = (self.prefixes.iter())
					.filter(|(id, _)| !remove.contains(id))
					.map(|(_, asked)| asked.path.clone())
					.collect();
				let mut adds = Vec::new();
				for (index, hidden) in add {
					let prefix = path(PREFIXES[*index]);
					if set
						.iter()
						.all(|other| !other.has_prefix(&prefix) && !prefix.has_prefix(other))
					{
						set.push(prefix.clone());
						adds.push((prefix, *hidden));
					}
				}
				let add = adds;
				let asked = self.queues(|sim| sim.answer.request(false, remove.clone(), add.clone()));
				prop_assert!(asked.is_ok(), "a valid edit is refused: {asked:?}");
				{
					let mut next = self.next_prefix;
					let add = (add.into_iter())
						.map(|(path, hidden)| {
							next += 1;
							(next - 1, path, hidden)
						})
						.collect();
					self.next_prefix = next;
					self.edit = Some(Edit {
						remove,
						add,
						at: self.step,
					});
				}
			}
			Op::Main | Op::Mount(_) => {
				self.send(op)?;
				return Ok(());
			}
			Op::Drain => {
				while self.send(&Op::Main)? {}
				return Ok(());
			}
		}
		// The origin feeds its node what changed, as its driver would.
		self.origin.route_node();
		flush(&mut self.writer);
		self.read()
	}

	/// Send one thing the session's key, or the `nth` mount, queued, returning whether there
	/// was one. The key waits while it is held.
	fn send(&mut self, op: &Op) -> Result<bool, TestCaseError> {
		let waiter = kio::Waiter::noop();
		self.answer.main.flush();
		let (source, msg) = match op {
			Op::Main if self.answer.holding() => return Ok(false),
			Op::Main => match self.answer.main.try_wire() {
				Some(msg) => (super::MAIN, msg),
				None => return Ok(false),
			},
			Op::Mount(nth) => {
				let Some(source) = (self.answer.mounts.keys())
					.nth(nth % self.answer.mounts.len().max(1))
					.copied()
				else {
					return Ok(false);
				};
				match self.answer.mounts[&source].stream.poll_wire(&waiter) {
					std::task::Poll::Ready(msg) => (source, msg),
					std::task::Poll::Pending => return Ok(false),
				}
			}
			_ => unreachable!("only a drain sends"),
		};
		(self.answer.sourced(&mut self.writer, source, msg))
			.map_err(|err| TestCaseError::fail(format!("output: {err:?}")))?;
		self.origin.route_node();
		flush(&mut self.writer);
		self.read()?;
		Ok(true)
	}

	/// Read what the answer sent, checking each promise as the peer meets it.
	fn read(&mut self) -> Result<(), TestCaseError> {
		for msg in self.peer.take() {
			match msg {
				AnnounceBroadcast::Start { suffix, instance, .. } => {
					self.peer.start(&suffix, &instance);
				}
				AnnounceBroadcast::Change { id, .. } => {
					prop_assert!(self.peer.live.contains_key(&id), "a change to {id}, which is not live");
				}
				AnnounceBroadcast::End { id, lost } => {
					let (path, instance) = self.peer.end(id);
					let announced = self.claims.values().any(|held| held.instance == instance);
					prop_assert!(
						lost || !announced,
						"{path} ({instance}) ended as withdrawn while its publisher still announces it"
					);
				}
				AnnounceBroadcast::Ok => {
					let edit = self.edit.take().expect("an ANNOUNCE_OK answers an edit");
					for id in &edit.remove {
						self.synced.remove(id);
					}
					let removed: Vec<Path> = (edit.remove.iter())
						.filter_map(|id| self.prefixes.remove(id))
						.map(|asked| asked.path)
						.collect();
					self.peer.drop_within(&removed);
					for (id, path, hidden) in edit.add {
						self.prefixes.insert(
							id,
							Asked {
								path,
								hidden,
								at: edit.at,
							},
						);
					}
				}
				AnnounceBroadcast::Synced { prefix } => {
					prop_assert!(self.synced.insert(prefix), "a second SYNCED for {prefix}");
					self.synced(prefix)?;
				}
				msg => prop_assert!(false, "a claims-only peer is sent {msg:?}"),
			}
		}
		Ok(())
	}

	/// SYNCED for `id` went out: every claim the grant in effect shows under it, there before
	/// the read that tells it started, has had its Start. That read started with the edit
	/// adding the prefix, or with the latest grant change in effect that read it when the one
	/// before did not. A change that keeps the read only judges again what it told, so a claim
	/// announced after the read started is a live change, whenever it comes.
	fn synced(&self, id: u64) -> Result<(), TestCaseError> {
		let asked = self.prefixes.get(&id).expect("SYNCED names a prefix in effect");
		let effect = self.grants.len() - 1 - self.unanswered();
		// The grant the edit was read under, then each change answered or queued since.
		let first = (self.grants.iter().rposition(|(step, _)| *step <= asked.at))
			.unwrap_or(0)
			.min(effect);
		let views: Vec<View> = (self.grants[first..].iter())
			.map(|(_, grant)| shown(&self.consumer, *grant, &asked.path, asked.hidden))
			.collect();
		let view = |index: usize| &views[index - first];
		let held = self.peer.holds();
		for claim in &view(effect).shown {
			// A change queued behind the one in effect may only take claims away.
			if (effect + 1..self.grants.len()).any(|later| !view(later).shown.contains(claim)) {
				continue;
			}
			let (path, instance) = claim;
			// The read that tells it: the one it lies in, or the one it is presented at.
			let read =
				|index: usize| (view(index).reads.iter()).find(|read| path.has_prefix(read) || read.has_prefix(path));
			let since = (first + 1..=effect)
				.rev()
				.find(|index| read(*index).is_none_or(|read| !view(index - 1).reads.contains(read)))
				.map_or(asked.at, |index| self.grants[index].0);
			let old = (self.claims.values()).any(|held| held.instance == *instance && held.at < since);
			prop_assert!(
				!old || held.contains(claim),
				"SYNCED for {:?} went out before {path} ({instance}) under grant {:?}",
				asked.path,
				self.grants[effect].1.map(|index| GRANTS[index]),
			);
		}
		Ok(())
	}
}

fn run(case: &Case) -> Result<(), TestCaseError> {
	let mut sim = Sim::new(case);
	for (path, scope) in &case.claims {
		sim.apply(&Op::Announce {
			path: *path,
			scope: *scope,
		})?;
	}
	for op in &case.ops {
		sim.apply(op)?;
	}
	// Then everything queued goes out, one side drained before the other.
	loop {
		let mut sent = false;
		if !case.mounts_first {
			while sim.send(&Op::Main)? {
				sent = true;
			}
		}
		for nth in 0..sim.answer.mounts.len() {
			while sim.send(&Op::Mount(nth))? {
				sent = true;
			}
		}
		if !sent && !sim.send(&Op::Main)? {
			break;
		}
	}
	// Once everything is out, every prefix in effect has had its SYNCED.
	for (id, asked) in &sim.prefixes {
		prop_assert!(sim.synced.contains(id), "no SYNCED for {:?} once drained", asked.path);
	}
	Ok(())
}

/// Runs the cases `MOQ_ANSWER_SEED` and `MOQ_ANSWER_CASES` pick.
#[test]
fn synced_follows_what_the_grant_shows() {
	let setting = |name: &str, fallback: u64| std::env::var(name).map_or(fallback, |value| value.parse().expect(name));
	let seed = setting("MOQ_ANSWER_SEED", 0x616e73);
	let count = setting("MOQ_ANSWER_CASES", 1024) as u32;
	let mut runner = TestRunner::new(Config {
		cases: count,
		rng_seed: RngSeed::Fixed(seed),
		failure_persistence: None,
		..Default::default()
	});
	runner.run(&case(), |case| run(&case)).unwrap();
}

/// Each message `answer` sends once everything queued goes out, the session's key first.
fn sent(answer: &mut Answer, writer: &mut Writer, peer: &mut Peer) -> Vec<AnnounceBroadcast> {
	answer.drain(writer).unwrap();
	flush(writer);
	peer.take()
}

/// A root request under `a/**` still waiting on the mount at `a` when the grant moves to
/// `b/**` holds its SYNCED for the initial set of `b`, read in place of `a`.
#[test]
fn a_grant_change_before_synced_holds_it_for_what_is_read_instead() {
	let origin = Hop::new(1).unwrap().produce();
	let _bc = origin.announce("b/c", origin::RouteInfo::new()).unwrap();
	let _cam = origin.announce("s/cam", origin::RouteInfo::new()).unwrap();
	let consumer = origin
		.mount("a", "s")
		.unwrap()
		.scope("", &patterns(&["**"]))
		.unwrap()
		.consume();
	let auth = crate::auth::Link::new(false);
	auth.handle().authorize(&grant(1));
	let mut answer = Answer::alone_on(consumer, auth.clone());
	let (mut writer, mut peer) = stream();
	answer.request(false, Vec::new(), vec![(path(""), false)]).unwrap();
	// The session's key answers the edit and sends what it reads of `a` before the mount at
	// `a` sends its initial set; the grant moves meanwhile.
	let mut msgs = Vec::new();
	while let Some(msg) = answer.main.try_wire() {
		answer.output(&mut writer, msg).unwrap();
	}
	auth.handle().authorize(&grant(2));
	answer.poll_permit(&kio::Waiter::noop()).unwrap();
	msgs.extend(sent(&mut answer, &mut writer, &mut peer));
	assert_eq!(events(&mut peer, &msgs), ["b/c", "SYNCED 0"], "{msgs:?}");
}

/// What the peer reads in `msgs`, in order: the path of each Start, and each SYNCED.
fn events(peer: &mut Peer, msgs: &[AnnounceBroadcast]) -> Vec<String> {
	let mut events = Vec::new();
	for msg in msgs {
		match msg {
			AnnounceBroadcast::Start { suffix, instance, .. } => {
				events.push(peer.start(suffix, instance).0.to_string());
			}
			AnnounceBroadcast::End { id, .. } => {
				peer.end(*id);
			}
			AnnounceBroadcast::Synced { prefix } => events.push(format!("SYNCED {prefix}")),
			_ => {}
		}
	}
	events
}

/// A re-price queued before the grant revokes the claim is judged by the new grant, and
/// the claim it hides ends as lost, since its publisher never withdrew it.
#[test]
fn a_change_queued_before_a_revocation_ends_its_claim_as_lost() {
	let origin = Hop::new(1).unwrap().produce();
	let ac = origin.announce("a/c", origin::RouteInfo::new()).unwrap();
	let auth = crate::auth::Link::new(false);
	auth.handle().authorize(&grant(1));
	let mut answer = Answer::alone_on(origin.consume(), auth.clone());
	let (mut writer, mut peer) = stream();
	answer.request(false, Vec::new(), vec![(path(""), false)]).unwrap();
	let first = sent(&mut answer, &mut writer, &mut peer);
	let AnnounceBroadcast::Start { instance, .. } = first[1].clone() else {
		panic!("{first:?}");
	};
	ac.update(origin::RouteInfo::new().with_instance(instance, 0).with_cost(5))
		.unwrap();
	origin.route_node();
	auth.handle().authorize(&grant(2));
	answer.poll_permit(&kio::Waiter::noop()).unwrap();
	let revoked = sent(&mut answer, &mut writer, &mut peer);
	let ends: Vec<bool> = (revoked.iter())
		.filter_map(|msg| match msg {
			AnnounceBroadcast::End { lost, .. } => Some(*lost),
			_ => None,
		})
		.collect();
	assert_eq!(ends, [true], "{revoked:?}");
}

/// A claim the old grant hid inside a read the new grant keeps is started before the SYNCED
/// of a prefix that another read of the change releases first: `a` waits on the mount at
/// `a/drop` when the grant moves from `a/u/*/old` to `a/u/*/new`, and SYNCED for `a` follows
/// `a/u/z/new`, though the read of `a/v` finishes before that of `b`.
#[test]
fn a_grant_change_shows_what_it_keeps_before_synced() {
	let origin = Hop::new(1).unwrap().produce();
	let _new = origin.announce("a/u/z/new", origin::RouteInfo::new()).unwrap();
	let consumer = origin
		.mount("a/drop", "s")
		.unwrap()
		.scope("", &patterns(&["**"]))
		.unwrap()
		.consume();
	let auth = crate::auth::Link::new(false);
	let authorize = |subscribe: &[&str]| {
		auth.handle().authorize(&crate::auth::Grant {
			publish: Patterns::default(),
			subscribe: patterns(subscribe),
			expires: None,
		})
	};
	authorize(&["a/u/*/old", "a/drop/**"]);
	let mut answer = Answer::alone_on(consumer, auth.clone());
	let (mut writer, mut peer) = stream();
	answer
		.request(false, Vec::new(), vec![(path("a"), false), (path("b"), false)])
		.unwrap();
	// The session's key answers the edit; the mount at `a/drop` has yet to send its initial set.
	answer.main.flush();
	while let Some(msg) = answer.main.try_wire() {
		answer.output(&mut writer, msg).unwrap();
	}
	flush(&mut writer);
	let first = peer.take();
	assert_eq!(events(&mut peer, &first), ["SYNCED 1"], "{first:?}");

	authorize(&["a/u/*/new", "a/v/**", "b/**"]);
	answer.poll_permit(&kio::Waiter::noop()).unwrap();
	let regranted = sent(&mut answer, &mut writer, &mut peer);
	assert_eq!(
		events(&mut peer, &regranted),
		["a/u/z/new", "SYNCED 0"],
		"{regranted:?}"
	);
}
