//! Resume checked against a model of one logical track.
//!
//! Random sequences of route flips, route failures, give-ups, ranges, cache expiry, and
//! reader joins and leaves drive an origin's fronts on the simulated clock. Every reader's
//! groups, frames, drops, and end are checked against what one plain cursor on the logical
//! track would see: every group its range and budget allow, each once, in its order. A
//! newer publisher epoch wins the path while older readers stay on theirs until it has no
//! route left, then end with an error.
//!
//! `MOQ_RESUME_SEED` and `MOQ_RESUME_CASES` pick the cases; nightly sweeps a fresh seed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestRunner};

use super::origin::{Producer, Route};
use crate::{Epoch, Error, Hop, Hops, SessionError, StreamError, Timestamp, broadcast, group, origin, track};

/// Groups in the logical track; group `g` has `1 + g % 3` frames.
const GROUPS: u64 = 12;
/// Routes per epoch, cheaper the higher the slot.
const SLOTS: usize = 3;
/// The older publisher instance, then a newer one.
const EPOCHS: usize = 2;
const READERS: usize = 4;
const PATH: &str = "room/alice";
/// A budget nothing in a case outlives.
const HOUR: Duration = Duration::from_secs(3600);
/// A budget spanning about three groups.
const SHORT: Duration = Duration::from_millis(30);

fn frames(group: u64) -> usize {
	1 + (group % 3) as usize
}

/// Frame `frame` of `group` as `epoch` publishes it: every route of one epoch carries it.
fn payload(epoch: usize, group: u64, frame: usize) -> Vec<u8> {
	format!("e{epoch}:{group}.{frame}").into_bytes()
}

fn epoch(index: usize) -> Epoch {
	// UUIDv7s minted in order, so the second is the newer instance.
	[
		"01900000-0000-7000-8000-000000000001",
		"01900000-0001-7000-8000-000000000001",
	][index]
		.parse()
		.unwrap()
}

/// How a route fails: its session closes on the far side.
fn route_failure() -> Error {
	Error::Stream(StreamError::Session(SessionError::Internal))
}

/// A verdict on content, not a route failing: another route may still deliver it.
const GIVE_UP: StreamError = StreamError::DeliveryTimeout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Budget {
	Hour,
	Short,
}

impl Budget {
	fn duration(self) -> Duration {
		match self {
			Self::Hour => HOUR,
			Self::Short => SHORT,
		}
	}
}

#[derive(Clone, Copy, Debug)]
struct Spec {
	/// Sequence order through [`track::Ordered`] rather than arrival order.
	ordered: bool,
	budget: Budget,
	/// The subscription's one group range, or the default.
	range: Option<(u64, u64)>,
	/// Polls drops from the start, as a publisher forwarding them does.
	drops: bool,
}

#[derive(Clone, Debug)]
enum Op {
	/// The route announces itself to the origin.
	Up {
		epoch: usize,
		slot: usize,
	},
	/// The route's session fails, failing its copy.
	Down {
		epoch: usize,
		slot: usize,
	},
	/// The route's copy gets up to `frames` more frames of `group`, or of the lowest group
	/// it lacks, finishing the group once complete.
	Write {
		epoch: usize,
		slot: usize,
		group: Option<u64>,
		frames: usize,
	},
	/// The route gives up `group` with a verdict on its content.
	GiveUp {
		epoch: usize,
		slot: usize,
		group: u64,
	},
	/// The route's copy declares the track's end, once it holds the last group.
	End {
		epoch: usize,
		slot: usize,
	},
	Advance {
		ms: u64,
	},
	/// Long enough for idle cached groups to expire.
	Expire,
	Join {
		reader: usize,
		spec: Spec,
	},
	Leave {
		reader: usize,
	},
	/// Replace the subscription's group range.
	Ranges {
		reader: usize,
		range: Option<(u64, u64)>,
	},
	/// Replace the reader's local group bounds.
	Bounds {
		reader: usize,
		range: Option<(u64, u64)>,
	},
	/// Poll the reader whether or not it was woken.
	Read {
		reader: usize,
	},
}

#[derive(Clone, Debug)]
struct Case {
	ops: Vec<Op>,
	/// Every live route declares the end once it has caught up.
	finish: bool,
}

fn range() -> impl Strategy<Value = Option<(u64, u64)>> {
	proptest::option::of((0..GROUPS, 1..=GROUPS).prop_map(|(start, len)| (start, (start + len).min(GROUPS))))
}

fn op() -> impl Strategy<Value = Op> {
	let route = (0..EPOCHS, 0..SLOTS);
	let spec = (
		any::<bool>(),
		prop_oneof![3 => Just(Budget::Hour), 1 => Just(Budget::Short)],
		range(),
		any::<bool>(),
	)
		.prop_map(|(ordered, budget, range, drops)| Spec {
			ordered,
			budget,
			range,
			drops,
		});
	prop_oneof![
		30 => (route.clone(), proptest::option::weighted(0.3, 0..GROUPS), 1..=3usize)
			.prop_map(|((epoch, slot), group, frames)| Op::Write { epoch, slot, group, frames }),
		8 => route.clone().prop_map(|(epoch, slot)| Op::Up { epoch, slot }),
		6 => route.clone().prop_map(|(epoch, slot)| Op::Down { epoch, slot }),
		4 => (route.clone(), 0..GROUPS - 1).prop_map(|((epoch, slot), group)| Op::GiveUp { epoch, slot, group }),
		2 => route.prop_map(|(epoch, slot)| Op::End { epoch, slot }),
		6 => (1..=200u64).prop_map(|ms| Op::Advance { ms }),
		1 => Just(Op::Expire),
		10 => (0..READERS, spec).prop_map(|(reader, spec)| Op::Join { reader, spec }),
		3 => (0..READERS).prop_map(|reader| Op::Leave { reader }),
		3 => (0..READERS, range()).prop_map(|(reader, range)| Op::Ranges { reader, range }),
		2 => (0..READERS, range()).prop_map(|(reader, range)| Op::Bounds { reader, range }),
		8 => (0..READERS).prop_map(|reader| Op::Read { reader }),
	]
}

fn case() -> impl Strategy<Value = Case> {
	(proptest::collection::vec(op(), 1..64), any::<bool>()).prop_map(|(ops, finish)| Case { ops, finish })
}

/// One route's copy of the logical track, as its upstream publishes it.
struct Copy {
	track: track::Producer,
	/// Answers the front's requests for the route until the route goes.
	serving: moq_clock::sim::JoinHandle<()>,
	_broadcast: broadcast::Producer,
	/// Fetches of groups the copy lacks, which the route forwards to the publisher.
	fills: track::Requests,
	/// The cached copies of each group: the live feed's, and any a fetch for its tail filled.
	groups: BTreeMap<u64, Vec<Part>>,
	given_up: BTreeSet<u64>,
}

/// One cached copy of a group, from frame `from`.
struct Part {
	from: usize,
	/// One past the last frame written.
	written: usize,
	/// While the copy is open.
	writer: Option<group::Producer>,
}

impl Copy {
	/// Whether the route holds every frame of `group`.
	fn complete(&self, group: u64) -> bool {
		self.groups
			.get(&group)
			.is_some_and(|parts| parts.iter().any(|part| part.from == 0 && part.written == frames(group)))
	}
}

/// One publisher instance: its routes, and what any of them ever carried.
#[derive(Default)]
struct Instance {
	routes: [Option<Copy>; SLOTS],
	/// Most frames of each group any route carried.
	written: BTreeMap<u64, usize>,
	/// Groups some route finished.
	complete: BTreeSet<u64>,
	/// Groups some route gave up.
	given_up: BTreeSet<u64>,
	/// Some route declared the track's end.
	ended: bool,
}

impl Instance {
	fn live(&self) -> impl Iterator<Item = &Copy> {
		self.routes.iter().flatten()
	}

	/// The slot of the cheapest live route, which the origin serves the path from.
	fn winner(&self) -> Option<usize> {
		self.routes.iter().rposition(Option::is_some)
	}

	fn newest(&self) -> Option<u64> {
		self.written.last_key_value().map(|(group, _)| *group)
	}
}

#[derive(Default)]
struct Flag(AtomicBool);

impl std::task::Wake for Flag {
	fn wake(self: Arc<Self>) {
		self.0.store(true, Ordering::Relaxed);
	}
}

enum Cursor {
	Arrival(track::Subscriber),
	Ordered(track::Ordered),
}

/// A group a reader was handed, and how far it read it.
struct Reading {
	consumer: Option<group::Consumer>,
	frames: usize,
	end: Option<Result<(), Error>>,
}

struct Reader {
	instance: usize,
	spec: Spec,
	cursor: Option<Cursor>,
	control: track::Control,
	_broadcast: broadcast::Consumer,
	flag: Arc<Flag>,
	waiter: kio::Waiter,
	ranges: Option<(u64, u64)>,
	bounds: Option<(u64, u64)>,
	/// Every group handed out, in order.
	handed: Vec<u64>,
	reading: BTreeMap<u64, Reading>,
	/// Groups reported dropped.
	dropped: BTreeSet<u64>,
	/// Only groups past this one are owed: older ones were cached history at join, or
	/// may have expired.
	floor: Option<u64>,
	/// Its instance lost its last route while it read.
	doomed: bool,
	/// The newest group handed out when its routes last changed. A route change resumes
	/// from there, so a group below it that lands only afterwards may be lost.
	resumed: Option<u64>,
	end: Option<Result<(), Error>>,
	/// What every live route carried in full, in range, when the track ended cleanly.
	carried: BTreeSet<u64>,
}

impl Reader {
	fn covers(&self, group: u64) -> bool {
		let within = |range: Option<(u64, u64)>| range.is_none_or(|(start, end)| (start..end).contains(&group));
		within(self.ranges) && within(self.bounds)
	}

	/// Everything observed, to tell whether a poll found anything new.
	fn observed(&self) -> (usize, usize, bool, usize) {
		let frames = self
			.reading
			.values()
			.map(|read| read.frames + usize::from(read.end.is_some()))
			.sum();
		(self.handed.len(), frames, self.end.is_some(), self.dropped.len())
	}
}

fn subscription(budget: Budget, range: Option<(u64, u64)>) -> track::Subscription {
	let subscription = track::Subscription::default().with_max_delay(budget.duration());
	match range {
		Some((start, end)) => subscription.with_range(start..end),
		None => subscription,
	}
}

fn fail(reader: usize, what: impl std::fmt::Display) -> TestCaseError {
	TestCaseError::fail(format!("reader {reader}: {what}"))
}

struct Model {
	origin: Producer,
	consumer: origin::Consumer,
	instances: [Instance; EPOCHS],
	readers: [Option<Reader>; READERS],
}

impl Model {
	fn new() -> Self {
		let origin = origin::Config {
			hop: Hop::new(1).unwrap(),
			update_hold: Duration::ZERO,
			..Default::default()
		};
		let (origin, driver) = Producer::new(origin);
		moq_clock::sim::spawn(crate::time::run(driver));
		Self {
			consumer: origin.consume(),
			origin,
			instances: Default::default(),
			readers: Default::default(),
		}
	}

	async fn apply(&mut self, op: &Op) -> Result<(), TestCaseError> {
		// A reader moves only when its instance's winning route, the cheapest live one, changes.
		let rerouted = match *op {
			Op::Up { epoch, slot } | Op::Down { epoch, slot } => {
				let instance = &self.instances[epoch];
				let mut live: Vec<bool> = instance.routes.iter().map(Option::is_some).collect();
				live[slot] = matches!(op, Op::Up { .. });
				(live.iter().rposition(|live| *live) != instance.winner()).then_some(epoch)
			}
			_ => None,
		};
		if let Some(epoch) = rerouted {
			for reader in self
				.readers
				.iter_mut()
				.flatten()
				.filter(|reader| reader.instance == epoch)
			{
				reader.resumed = reader.resumed.max(reader.handed.iter().max().copied());
			}
		}
		match *op {
			Op::Up { epoch, slot } => self.up(epoch, slot),
			Op::Down { epoch, slot } => {
				let instance = &mut self.instances[epoch];
				let Some(copy) = instance.routes[slot].take() else {
					return Ok(());
				};
				copy.serving.abort();
				for part in copy.groups.into_values().flatten() {
					if let Some(writer) = part.writer {
						let _ = writer.abort(route_failure());
					}
				}
				let _ = copy.track.abort(route_failure());
				if instance.live().next().is_none() {
					for reader in self.readers.iter_mut().flatten() {
						if reader.instance == epoch {
							reader.doomed = true;
						}
					}
				}
			}
			Op::Write {
				epoch,
				slot,
				group,
				frames,
			} => {
				let Some(copy) = &self.instances[epoch].routes[slot] else {
					return Ok(());
				};
				let group = group
					.or_else(|| (0..GROUPS).find(|group| !copy.complete(*group) && !copy.given_up.contains(group)));
				if let Some(group) = group {
					self.write(epoch, slot, group, frames);
				}
			}
			Op::GiveUp { epoch, slot, group } => {
				let instance = &mut self.instances[epoch];
				let Some(copy) = &mut instance.routes[slot] else {
					return Ok(());
				};
				if copy.complete(group) || !copy.given_up.insert(group) {
					return Ok(());
				}
				match copy.groups.get_mut(&group) {
					Some(parts) => {
						for writer in parts.iter_mut().filter_map(|part| part.writer.take()) {
							let _ = writer.abort(Error::Stream(GIVE_UP));
						}
					}
					None => copy.track.drop_groups(group..group + 1, GIVE_UP),
				}
				instance.given_up.insert(group);
			}
			Op::End { epoch, slot } => self.end(epoch, slot),
			Op::Advance { ms } => moq_clock::sim::advance(Duration::from_millis(ms)).await,
			Op::Expire => {
				moq_clock::sim::advance(Duration::from_secs(60)).await;
				for reader in self.readers.iter_mut().flatten() {
					reader.floor = self.instances[reader.instance].newest();
				}
			}
			Op::Join { reader, spec } => self.join(reader, spec).await?,
			Op::Leave { reader } => self.readers[reader] = None,
			Op::Ranges { reader: index, range } => {
				let Some(reader) = &mut self.readers[index] else {
					return Ok(());
				};
				reader.ranges = range;
				let _ = reader.control.update(subscription(reader.spec.budget, range));
				self.poll(index)?;
			}
			Op::Bounds { reader: index, range } => {
				let Some(reader) = &mut self.readers[index] else {
					return Ok(());
				};
				reader.bounds = range;
				let (start, end) = range.unwrap_or((0, u64::MAX));
				reader.cursor = reader.cursor.take().map(|cursor| match cursor {
					Cursor::Arrival(sub) => Cursor::Arrival(sub.with_range(start..end)),
					Cursor::Ordered(sub) => Cursor::Ordered(sub.with_range(start..end)),
				});
				self.poll(index)?;
			}
			Op::Read { reader } => {
				if self.readers[reader].is_some() {
					self.poll(reader)?;
				}
			}
		}
		Ok(())
	}

	fn up(&mut self, epoch: usize, slot: usize) {
		if self.instances[epoch].routes[slot].is_some() {
			return;
		}
		let mut hops = Hops::new();
		hops.push(Hop::new(10 * (epoch as u64 + 1) + slot as u64).unwrap())
			.unwrap();
		let route = Route::default()
			.with_epoch(self::epoch(epoch))
			.with_hops(hops)
			.with_cost((SLOTS - slot) as u64);
		let requests = self.origin.requests_route(PATH, route, None).unwrap();
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track("video", track::Info::timed()).unwrap();
		let upstream = broadcast.consume();
		let serving = moq_clock::sim::spawn(async move {
			while let Ok(request) = kio::wait(|waiter| requests.poll_requested_broadcast(waiter)).await {
				request.accept(&upstream);
			}
		});
		self.instances[epoch].routes[slot] = Some(Copy {
			fills: track.requests(),
			track,
			serving,
			_broadcast: broadcast,
			groups: BTreeMap::new(),
			given_up: BTreeSet::new(),
		});
	}

	/// The route's live feed brings up to `more` frames of `group` past what it holds.
	fn write(&mut self, epoch: usize, slot: usize, group: u64, more: usize) {
		let Some(copy) = &mut self.instances[epoch].routes[slot] else {
			return;
		};
		if copy.given_up.contains(&group) {
			return;
		}
		let parts = copy.groups.entry(group).or_default();
		if !parts.iter().any(|part| part.from == 0)
			&& let Ok(writer) = copy.track.create_group(group::Info { sequence: group })
		{
			parts.push(Part {
				from: 0,
				written: 0,
				writer: Some(writer),
			});
		}
		let held = parts.iter().map(|part| part.written).max().unwrap_or(0);
		self.catch_up(epoch, slot, group, held.saturating_add(more));
	}

	/// Write every open copy of `group` on the route up to frame `until`.
	fn catch_up(&mut self, epoch: usize, slot: usize, group: u64, until: usize) {
		let instance = &mut self.instances[epoch];
		let Some(parts) = instance.routes[slot]
			.as_mut()
			.and_then(|copy| copy.groups.get_mut(&group))
		else {
			return;
		};
		let until = until.min(frames(group));
		for part in parts.iter_mut() {
			let Some(writer) = &mut part.writer else { continue };
			while part.written < until {
				let at = Timestamp::from_millis(group * 10 + part.written as u64).unwrap();
				writer.write_frame(at, payload(epoch, group, part.written)).unwrap();
				part.written += 1;
			}
			if part.written == frames(group) {
				part.writer.take().unwrap().finish().unwrap();
				instance.complete.insert(group);
			}
			let most = instance.written.entry(group).or_default();
			*most = (*most).max(part.written);
		}
	}

	/// Answer each route's fetches as its publisher would: with what the instance carries
	/// so far, which later writes continue, or not found when no route has it yet.
	fn fill(&mut self) -> bool {
		let mut filled = false;
		for epoch in 0..EPOCHS {
			for slot in 0..SLOTS {
				while let Some(Poll::Ready(Ok(fill))) = self.instances[epoch].routes[slot]
					.as_ref()
					.map(|copy| copy.fills.poll_requested_fill(&kio::Waiter::noop()))
				{
					filled = true;
					let (start, end) = (fill.start().group, fill.end());
					for group in start..end.min(GROUPS) {
						let instance = &mut self.instances[epoch];
						let copy = instance.routes[slot].as_mut().expect("polled above");
						let from = fill.frame_start(group) as usize;
						let written = instance.written.get(&group).copied().unwrap_or(0);
						let held = copy.groups.get(&group).into_iter().flatten();
						if copy.given_up.contains(&group) {
							fill.refuse_groups(group..group + 1, Error::Stream(GIVE_UP));
						} else if written == 0 || written < from || held.into_iter().any(|part| part.from <= from) {
							fill.refuse_groups(group..group + 1, Error::NotFound);
						} else if let Ok(writer) = fill.accept(group, None) {
							// The fill starts at the frame its readers want, which may not exist yet.
							copy.groups.entry(group).or_default().push(Part {
								from,
								written: from,
								writer: Some(writer),
							});
							self.catch_up(epoch, slot, group, written);
						}
					}
					if end > GROUPS {
						fill.refuse_groups(GROUPS.max(start)..end, Error::NotFound);
					}
				}
			}
		}
		filled
	}

	fn end(&mut self, epoch: usize, slot: usize) {
		let instance = &mut self.instances[epoch];
		let Some(copy) = &instance.routes[slot] else {
			return;
		};
		if copy.groups.contains_key(&(GROUPS - 1)) && copy.track.finish().is_ok() {
			instance.ended = true;
		}
	}

	async fn join(&mut self, index: usize, spec: Spec) -> Result<(), TestCaseError> {
		if self.readers[index].is_some() {
			return Ok(());
		}
		// The newest instance with a route wins the path.
		let Some(expected) = (0..EPOCHS)
			.rev()
			.find(|epoch| self.instances[*epoch].live().next().is_some())
		else {
			return Ok(());
		};
		let broadcast = self
			.consumer
			.request(PATH, None)
			.await
			.map_err(|err| fail(index, format!("request failed: {err}")))?;
		let instance = (0..EPOCHS)
			.find(|epoch| broadcast.info().epoch.as_ref() == Some(&self::epoch(*epoch)))
			.ok_or_else(|| fail(index, "resolved no known instance"))?;
		if instance != expected {
			return Err(fail(
				index,
				format!("joined instance {instance}, not the newest {expected}"),
			));
		}
		let subscriber = broadcast
			.track("video")
			.map_err(|err| fail(index, format!("track: {err}")))?
			.subscribe(subscription(spec.budget, spec.range))
			.await
			.map_err(|err| fail(index, format!("subscribe failed: {err}")))?;
		let flag = Arc::<Flag>::default();
		let reader = Reader {
			instance,
			spec,
			control: subscriber.control(),
			cursor: Some(match spec.ordered {
				true => Cursor::Ordered(subscriber.ordered()),
				false => Cursor::Arrival(subscriber),
			}),
			_broadcast: broadcast,
			waiter: kio::Waiter::new(flag.clone().into()),
			flag,
			ranges: spec.range,
			bounds: None,
			handed: Vec::new(),
			reading: BTreeMap::new(),
			dropped: BTreeSet::new(),
			floor: self.instances[instance].newest(),
			doomed: false,
			resumed: None,
			end: None,
			carried: BTreeSet::new(),
		};
		self.readers[index] = Some(reader);
		self.poll(index)
	}

	/// Read everything `index` has ready, checking each step against the model.
	fn poll(&mut self, index: usize) -> Result<(), TestCaseError> {
		let Some(reader) = &mut self.readers[index] else {
			return Ok(());
		};
		let instance = &self.instances[reader.instance];
		reader.flag.0.store(false, Ordering::Relaxed);
		let waiter = reader.waiter.clone();

		while reader.end.is_none() {
			let polled = match reader.cursor.as_mut().unwrap() {
				Cursor::Arrival(sub) => sub.poll_recv_group(&waiter),
				Cursor::Ordered(sub) => sub.poll_next_group(&waiter),
			};
			match polled {
				Poll::Pending => break,
				Poll::Ready(Ok(Some(group))) => {
					let sequence = group.sequence;
					if reader.reading.contains_key(&sequence) {
						return Err(fail(
							index,
							format!("group {sequence} handed out twice: {:?}", reader.handed),
						));
					}
					if reader.spec.ordered && reader.handed.last().is_some_and(|last| *last >= sequence) {
						return Err(fail(
							index,
							format!("group {sequence} out of order after {:?}", reader.handed),
						));
					}
					if !reader.covers(sequence) {
						return Err(fail(
							index,
							format!(
								"group {sequence} outside ranges {:?} and bounds {:?}",
								reader.ranges, reader.bounds
							),
						));
					}
					if !instance.written.contains_key(&sequence) {
						return Err(fail(
							index,
							format!("group {sequence} handed out before any route had it"),
						));
					}
					reader.handed.push(sequence);
					reader.reading.insert(
						sequence,
						Reading {
							consumer: Some(group),
							frames: 0,
							end: None,
						},
					);
				}
				Poll::Ready(Ok(None)) => {
					if !instance.ended {
						return Err(fail(index, "the track ended cleanly though no route declared an end"));
					}
					// A group landing below the end afterwards is not owed: a reader stops at it.
					reader.carried = (0..GROUPS)
						.filter(|group| reader.covers(*group) && instance.live().all(|copy| copy.complete(*group)))
						.collect();
					reader.end = Some(Ok(()));
				}
				Poll::Ready(Err(err)) => {
					if !reader.doomed {
						return Err(fail(
							index,
							format!("the track failed while its instance had a route: {err}"),
						));
					}
					reader.end = Some(Err(err));
				}
			}
		}

		if reader.spec.drops
			&& let Some(Cursor::Arrival(sub)) = &mut reader.cursor
		{
			while let Poll::Ready((range, err)) = sub.poll_drop(&waiter) {
				for group in range.clone() {
					if reader.reading.contains_key(&group) {
						return Err(fail(
							index,
							format!("dropped {range:?} ({err}) after handing out {group}"),
						));
					}
				}
				reader.dropped.extend(range.start..range.end.min(GROUPS));
			}
		}

		for (sequence, read) in &mut reader.reading {
			let Some(consumer) = &mut read.consumer else {
				continue;
			};
			loop {
				match consumer.poll_read_frame(&waiter) {
					Poll::Pending => break,
					Poll::Ready(Ok(Some(frame))) => {
						let expected = payload(reader.instance, *sequence, read.frames);
						if frame.payload[..] != expected[..] || read.frames >= frames(*sequence) {
							return Err(fail(
								index,
								format!(
									"group {sequence} frame {} read {:?}, expected {:?}",
									read.frames,
									String::from_utf8_lossy(&frame.payload),
									String::from_utf8_lossy(&expected)
								),
							));
						}
						read.frames += 1;
					}
					Poll::Ready(Ok(None)) => {
						if read.frames != frames(*sequence) || !instance.complete.contains(sequence) {
							return Err(fail(
								index,
								format!("group {sequence} ended cleanly after {} frames", read.frames),
							));
						}
						read.end = Some(Ok(()));
						read.consumer = None;
						break;
					}
					Poll::Ready(Err(err)) => {
						read.end = Some(Err(err));
						read.consumer = None;
						break;
					}
				}
			}
		}
		Ok(())
	}

	/// Let the driver and the routes run without time passing, polling each reader woken.
	async fn settle(&mut self) -> Result<(), TestCaseError> {
		let mut quiet = 0;
		for _ in 0..10_000 {
			moq_clock::sim::yield_now().await;
			let mut woken = self.fill();
			for index in 0..READERS {
				if self.readers[index]
					.as_ref()
					.is_some_and(|reader| reader.flag.0.load(Ordering::Relaxed))
				{
					woken = true;
					self.poll(index)?;
				}
			}
			quiet = if woken { 0 } else { quiet + 1 };
			if quiet == 16 {
				return Ok(());
			}
		}
		Err(TestCaseError::fail(
			"readers never settled: something keeps waking them",
		))
	}

	/// Every live route catches up on every group and, for `finish`, declares the end; then
	/// each reader must have read what it is owed.
	async fn conclude(&mut self, finish: bool) -> Result<(), TestCaseError> {
		let before: Vec<Option<u64>> = self
			.readers
			.iter()
			.map(|reader| reader.as_ref().and_then(|reader| reader.handed.iter().max().copied()))
			.collect();
		for epoch in 0..EPOCHS {
			for slot in 0..SLOTS {
				for group in 0..GROUPS {
					self.write(epoch, slot, group, usize::MAX);
				}
				if finish {
					self.end(epoch, slot);
				}
			}
		}
		self.settle().await?;

		let observed: Vec<_> = self
			.readers
			.iter()
			.map(|reader| reader.as_ref().map(Reader::observed))
			.collect();
		for index in 0..READERS {
			if self.readers[index].is_some() {
				self.poll(index)?;
			}
		}
		for (index, reader) in self.readers.iter().enumerate() {
			let Some(reader) = reader else { continue };
			if Some(reader.observed()) != observed[index] {
				return Err(fail(
					index,
					format!(
						"a poll found progress that never woke it: {:?} -> {:?}, handed {:?}, dropped {:?}, end {:?}",
						observed[index],
						reader.observed(),
						reader.handed,
						reader.dropped,
						reader.end
					),
				));
			}
			self.owed(index, reader, before[index], finish)?;
		}
		Ok(())
	}

	fn owed(&self, index: usize, reader: &Reader, before: Option<u64>, finish: bool) -> Result<(), TestCaseError> {
		let instance = &self.instances[reader.instance];
		if reader.doomed {
			return match &reader.end {
				Some(Err(_)) => Ok(()),
				Some(Ok(())) if instance.ended => Ok(()),
				end => Err(fail(index, format!("outlived its instance's routes: {end:?}"))),
			};
		}
		// What every live route carries in full.
		let carried = |group: u64| instance.live().all(|copy| copy.complete(group));
		let mut owed: Vec<u64> = (0..GROUPS)
			.filter(|group| reader.floor.is_none_or(|floor| *group > floor))
			.filter(|group| reader.resumed.is_none_or(|resumed| *group > resumed))
			.filter(|group| reader.covers(*group) && carried(*group))
			.collect();
		if reader.spec.ordered {
			owed.retain(|group| before.is_none_or(|before| *group > before));
		}
		if matches!(reader.end, Some(Ok(()))) {
			owed.retain(|group| reader.carried.contains(group));
		}
		if reader.spec.budget == Budget::Short {
			owed.retain(|group| *group == GROUPS - 1);
		}
		let missing: Vec<u64> = owed
			.iter()
			.copied()
			.filter(|group| !reader.reading.contains_key(group))
			.collect();
		let mut problems = String::new();
		if !missing.is_empty() {
			let _ = write!(problems, "missing groups {missing:?}; ");
		}
		if reader.spec.budget == Budget::Hour {
			for (sequence, read) in &reader.reading {
				if carried(*sequence) && !instance.given_up.contains(sequence) && !matches!(read.end, Some(Ok(()))) {
					let _ = write!(
						problems,
						"group {sequence} stopped at frame {} ({:?}); ",
						read.frames, read.end
					);
				}
			}
		}
		match (&reader.end, finish && instance.live().next().is_some()) {
			(None, false) | (Some(Ok(())), true) => {}
			(Some(Ok(())), false) if instance.ended => {}
			(end, _) => {
				let _ = write!(problems, "track end {end:?}; ");
			}
		}
		if problems.is_empty() {
			return Ok(());
		}
		Err(fail(
			index,
			format!(
				"{problems}handed out {:?}, dropped {:?}, floor {:?}, spec {:?}",
				reader.handed, reader.dropped, reader.floor, reader.spec
			),
		))
	}
}

fn run(case: &Case) -> Result<(), TestCaseError> {
	moq_clock::sim::run(async {
		let mut model = Model::new();
		model.up(0, 0);
		for op in &case.ops {
			model.apply(op).await?;
			model.settle().await?;
		}
		model.conclude(case.finish).await
	})
}

/// Runs the cases `MOQ_RESUME_SEED` and `MOQ_RESUME_CASES` pick.
#[test]
fn readers_match_the_logical_track() {
	let setting = |name: &str, fallback: u64| std::env::var(name).map_or(fallback, |value| value.parse().expect(name));
	let seed = setting("MOQ_RESUME_SEED", 0x6d6f71);
	let count = setting("MOQ_RESUME_CASES", 64) as u32;
	eprintln!("MOQ_RESUME_SEED={seed} MOQ_RESUME_CASES={count}");
	let mut runner = TestRunner::new(Config {
		cases: count,
		rng_seed: RngSeed::Fixed(seed),
		failure_persistence: None,
		..Default::default()
	});
	runner.run(&case(), |case| run(&case)).unwrap();
}

/// An Up or Down that leaves the winning route serving resumes nothing, so lower groups stay owed.
#[test]
fn unchanged_routes_keep_groups_owed() {
	moq_clock::sim::run(async {
		let mut model = Model::new();
		model.up(0, 0);
		let ops = [
			Op::Up { epoch: 0, slot: 2 },
			Op::Join {
				reader: 0,
				spec: Spec {
					ordered: false,
					budget: Budget::Hour,
					range: None,
					drops: false,
				},
			},
			Op::Write {
				epoch: 0,
				slot: 2,
				group: Some(8),
				frames: 1,
			},
			// Repeated, absent, and standby routes.
			Op::Up { epoch: 0, slot: 2 },
			Op::Down { epoch: 0, slot: 1 },
			Op::Up { epoch: 0, slot: 1 },
			Op::Down { epoch: 0, slot: 1 },
			Op::Down { epoch: 0, slot: 0 },
		];
		for op in &ops {
			model.apply(op).await.unwrap();
			model.settle().await.unwrap();
		}
		let reader = model.readers[0].as_ref().unwrap();
		assert!(reader.handed.contains(&8), "handed out {:?}", reader.handed);
		assert_eq!(reader.resumed, None);
	});
}

/// Failures the sweep found, as it shrank them.
#[test]
fn regressions() {
	let spec = |ordered, budget, range, drops| Spec {
		ordered,
		budget,
		range,
		drops,
	};
	let cases = [
		// An ordered reader whose range a finished track already passed ends.
		Case {
			ops: vec![Op::Join {
				reader: 0,
				spec: spec(true, Budget::Hour, Some((0, 1)), false),
			}],
			finish: true,
		},
		// An ordered reader ends once the rest of its range was given up.
		Case {
			ops: vec![
				Op::Up { epoch: 1, slot: 2 },
				Op::Join {
					reader: 0,
					spec: spec(true, Budget::Hour, Some((0, 8)), false),
				},
				Op::GiveUp {
					epoch: 1,
					slot: 2,
					group: 7,
				},
			],
			finish: true,
		},
		// Local bounds below the subscription's range leave no drops to report.
		Case {
			ops: vec![
				Op::Join {
					reader: 0,
					spec: spec(false, Budget::Hour, Some((11, 12)), true),
				},
				Op::Write {
					epoch: 0,
					slot: 0,
					group: None,
					frames: 1,
				},
				Op::Bounds {
					reader: 0,
					range: Some((0, 1)),
				},
			],
			finish: false,
		},
		// A drop held for a replaced copy is reported once that copy only backs a read.
		Case {
			ops: vec![
				Op::Join {
					reader: 0,
					spec: spec(false, Budget::Short, None, true),
				},
				Op::Write {
					epoch: 0,
					slot: 0,
					group: None,
					frames: 1,
				},
				Op::Up { epoch: 0, slot: 2 },
				Op::Write {
					epoch: 0,
					slot: 2,
					group: Some(6),
					frames: 1,
				},
			],
			finish: false,
		},
	];
	for case in cases {
		if let Err(err) = run(&case) {
			panic!("{case:?}: {err}");
		}
	}
}
